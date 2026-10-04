//! Playing a tag through the system audio player (`paplay`, `pw-play`, `aplay`, `afplay`, or a
//! custom command). All platform differences in playback live here.

use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use tracing::info;

use crate::config::{Config, PlayerSetting};
use crate::exec::{Cmd, ExecError};

pub trait Player {
    fn play(&self, file: &Path) -> Result<(), ExecError>;

    /// Program that must be installed (`None` for players that need nothing).
    fn program(&self) -> Option<&str>;

    /// Whether the player talks to a PulseAudio/PipeWire server (so `pactl info` is a useful
    /// preflight check).
    fn uses_pulse(&self) -> bool {
        false
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Paplay,
    PwPlay,
    Aplay,
    Afplay,
}

impl Backend {
    pub fn program(self) -> &'static str {
        match self {
            Self::Paplay => "paplay",
            Self::PwPlay => "pw-play",
            Self::Aplay => "aplay",
            Self::Afplay => "afplay",
        }
    }

    /// Arguments for playing `file` at `volume` (0–100).
    fn args(self, file: &Path, volume: u8) -> Vec<OsString> {
        let volume = u32::from(volume.min(100));
        let mut args: Vec<OsString> = match self {
            // 65536 = 100 % (PA_VOLUME_NORM).
            Self::Paplay => vec![format!("--volume={}", volume * 65536 / 100).into()],
            Self::PwPlay => vec![format!("--volume={:.2}", f64::from(volume) / 100.0).into()],
            // ALSA has no per-stream volume.
            Self::Aplay => vec!["-q".into()],
            // 1.0 = unchanged level.
            Self::Afplay => vec![
                "-v".into(),
                format!("{:.2}", f64::from(volume) / 100.0).into(),
            ],
        };
        args.push(file.as_os_str().to_owned());
        args
    }
}

/// Plays through an external program, killed after `timeout`.
#[derive(Debug, Clone)]
pub struct CommandPlayer {
    kind: Kind,
    volume: u8,
    timeout: Duration,
}

#[derive(Debug, Clone)]
enum Kind {
    Builtin(Backend),
    /// `PLAYER_COMMAND` argv with `{file}` / `{volume}` placeholders.
    Custom(Vec<String>),
}

impl CommandPlayer {
    pub fn builtin(backend: Backend, volume: u8, timeout: Duration) -> Self {
        Self {
            kind: Kind::Builtin(backend),
            volume,
            timeout,
        }
    }

    pub fn custom(argv: Vec<String>, volume: u8, timeout: Duration) -> Self {
        Self {
            kind: Kind::Custom(argv),
            volume,
            timeout,
        }
    }

    /// The command that plays `file`. Never goes through a shell.
    pub fn command(&self, file: &Path) -> Cmd {
        match &self.kind {
            Kind::Builtin(backend) => {
                Cmd::new(backend.program()).args(backend.args(file, self.volume))
            }
            Kind::Custom(argv) => {
                let volume = self.volume.to_string();
                let args = argv[1..].iter().map(|arg| -> OsString {
                    if arg == "{file}" {
                        // Exact placeholder: pass the path through untouched (may be non-UTF-8).
                        file.as_os_str().to_owned()
                    } else {
                        arg.replace("{file}", &file.to_string_lossy())
                            .replace("{volume}", &volume)
                            .into()
                    }
                });
                Cmd::new(&argv[0]).args(args)
            }
        }
        .timeout(self.timeout)
    }
}

impl Player for CommandPlayer {
    fn play(&self, file: &Path) -> Result<(), ExecError> {
        self.command(file).run().map(|_| ())
    }

    fn program(&self) -> Option<&str> {
        Some(match &self.kind {
            Kind::Builtin(backend) => backend.program(),
            Kind::Custom(argv) => &argv[0],
        })
    }

    fn uses_pulse(&self) -> bool {
        matches!(self.kind, Kind::Builtin(Backend::Paplay | Backend::PwPlay))
    }
}

/// `PLAYER=none` / `--silent` / `--dry-run`: logs instead of playing.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullPlayer;

impl Player for NullPlayer {
    fn play(&self, file: &Path) -> Result<(), ExecError> {
        info!(tag = %file.display(), "would play (no audio player)");
        Ok(())
    }

    fn program(&self) -> Option<&str> {
        None
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PlayerError {
    #[error(
        "no audio player found: install pulseaudio-utils (paplay), pipewire (pw-play) or \
         alsa-utils (aplay), or set PLAYER"
    )]
    NoneFound,
}

/// The player for the configured `PLAYER`.
pub fn from_config(config: &Config) -> Result<Box<dyn Player>, PlayerError> {
    let builtin = |backend| -> Box<dyn Player> {
        Box::new(CommandPlayer::builtin(
            backend,
            config.volume,
            config.play_timeout,
        ))
    };
    Ok(match &config.player {
        PlayerSetting::Auto => builtin(
            auto_backend(|p| crate::exec::find_program(p).is_some())
                .ok_or(PlayerError::NoneFound)?,
        ),
        PlayerSetting::Paplay => builtin(Backend::Paplay),
        PlayerSetting::PwPlay => builtin(Backend::PwPlay),
        PlayerSetting::Aplay => builtin(Backend::Aplay),
        PlayerSetting::Afplay => builtin(Backend::Afplay),
        PlayerSetting::Command(argv) => Box::new(CommandPlayer::custom(
            argv.clone(),
            config.volume,
            config.play_timeout,
        )),
        PlayerSetting::None => Box::new(NullPlayer),
    })
}

/// `PLAYER=auto`: `afplay` on macOS; on Linux the first installed of `paplay` (PulseAudio and
/// PipeWire-pulse), `pw-play`, `aplay`.
pub fn auto_backend(installed: impl Fn(&str) -> bool) -> Option<Backend> {
    let candidates: &[Backend] = if cfg!(target_os = "macos") {
        &[Backend::Afplay]
    } else {
        &[Backend::Paplay, Backend::PwPlay, Backend::Aplay]
    };
    candidates.iter().copied().find(|b| installed(b.program()))
}

#[cfg(test)]
pub mod testing {
    use std::cell::RefCell;
    use std::path::PathBuf;

    use super::*;

    /// Records every file it is asked to play; can be told to fail the first `n` plays.
    #[derive(Default)]
    pub struct RecordingPlayer {
        pub played: RefCell<Vec<PathBuf>>,
        pub fail_first: RefCell<usize>,
    }

    impl RecordingPlayer {
        pub fn failing(n: usize) -> Self {
            Self {
                fail_first: RefCell::new(n),
                ..Self::default()
            }
        }

        pub fn played(&self) -> Vec<PathBuf> {
            self.played.borrow().clone()
        }
    }

    impl Player for RecordingPlayer {
        fn play(&self, file: &Path) -> Result<(), ExecError> {
            self.played.borrow_mut().push(file.to_owned());
            let mut fail = self.fail_first.borrow_mut();
            if *fail > 0 {
                *fail -= 1;
                return Err(ExecError::Timeout {
                    command: "fake".into(),
                    timeout: Duration::from_secs(1),
                });
            }
            Ok(())
        }

        fn program(&self) -> Option<&str> {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(player: &CommandPlayer) -> Vec<String> {
        let cmd = player.command(Path::new("/tags/default.wav"));
        std::iter::once(cmd.program())
            .chain(cmd.arguments().iter().map(|a| a.as_os_str()))
            .map(|s| s.to_string_lossy().into_owned())
            .collect()
    }

    fn builtin(backend: Backend, volume: u8) -> Vec<String> {
        argv(&CommandPlayer::builtin(
            backend,
            volume,
            Duration::from_secs(15),
        ))
    }

    #[test]
    fn builtin_argv_and_volume() {
        assert_eq!(
            builtin(Backend::Paplay, 100),
            ["paplay", "--volume=65536", "/tags/default.wav"]
        );
        assert_eq!(
            builtin(Backend::Paplay, 50),
            ["paplay", "--volume=32768", "/tags/default.wav"]
        );
        assert_eq!(
            builtin(Backend::Paplay, 0),
            ["paplay", "--volume=0", "/tags/default.wav"]
        );
        assert_eq!(
            builtin(Backend::PwPlay, 75),
            ["pw-play", "--volume=0.75", "/tags/default.wav"]
        );
        assert_eq!(
            builtin(Backend::Aplay, 10),
            ["aplay", "-q", "/tags/default.wav"]
        );
        assert_eq!(
            builtin(Backend::Afplay, 100),
            ["afplay", "-v", "1.00", "/tags/default.wav"]
        );
    }

    #[test]
    fn custom_command_substitutes_without_shell() {
        let player = CommandPlayer::custom(
            vec![
                "ffplay".into(),
                "-nodisp".into(),
                "-volume".into(),
                "{volume}".into(),
                "{file}".into(),
                "--label=file:{file}".into(),
            ],
            40,
            Duration::from_secs(5),
        );
        assert_eq!(
            argv(&player),
            [
                "ffplay",
                "-nodisp",
                "-volume",
                "40",
                "/tags/default.wav",
                "--label=file:/tags/default.wav"
            ]
        );
        assert_eq!(player.program(), Some("ffplay"));
        assert!(!player.uses_pulse());
    }

    #[test]
    fn file_names_with_shell_characters_stay_one_argument() {
        let player = CommandPlayer::builtin(Backend::Aplay, 100, Duration::from_secs(1));
        let cmd = player.command(Path::new("/tags/a b; rm -rf ~.wav"));
        assert_eq!(cmd.arguments().last().unwrap(), "/tags/a b; rm -rf ~.wav");
    }

    #[test]
    fn auto_picks_first_installed() {
        if cfg!(target_os = "macos") {
            assert_eq!(auto_backend(|_| true), Some(Backend::Afplay));
        } else {
            assert_eq!(auto_backend(|_| true), Some(Backend::Paplay));
            assert_eq!(auto_backend(|p| p != "paplay"), Some(Backend::PwPlay));
            assert_eq!(auto_backend(|p| p == "aplay"), Some(Backend::Aplay));
        }
        assert_eq!(auto_backend(|_| false), None);
    }

    #[test]
    fn player_is_killed_after_timeout() {
        let player = CommandPlayer::custom(
            vec!["sleep".into(), "5".into(), "{file}".into()],
            100,
            Duration::from_millis(100),
        );
        // `sleep 5 1` sleeps for 6 s unless killed.
        let started = std::time::Instant::now();
        assert!(matches!(
            player.play(Path::new("1")),
            Err(ExecError::Timeout { .. })
        ));
        assert!(started.elapsed() < Duration::from_secs(4));
    }
}
