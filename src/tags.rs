//! Producer tag files: lookup per author, `tag set` and `tag list`.
//!
//! Author names come from the API (or the command line), so they are validated before they
//! become part of a path: a tag can never be read or written outside `TAGS_DIR`.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use crate::source::Platform;

/// Tried in this order.
pub const EXTENSIONS: [&str; 6] = ["wav", "ogg", "flac", "mp3", "aiff", "m4a"];
pub const MAX_TAG_BYTES: u64 = 5 * 1024 * 1024;
pub const DEFAULT_STEM: &str = "default";

#[derive(Debug, thiserror::Error)]
pub enum TagError {
    #[error("invalid author name {0:?}: use the GitHub login / GitLab username")]
    InvalidName(String),
    #[error("invalid owner {0:?}: expected <platform>:<user> (e.g. github:alice) or <user>")]
    InvalidOwner(String),
    #[error("unsupported format {0:?}: use one of {list}", list = EXTENSIONS.join(", "))]
    UnsupportedFormat(String),
    #[error("{} is {size} bytes; tags are limited to {} MB", path.display(), MAX_TAG_BYTES / 1024 / 1024)]
    TooLarge { path: PathBuf, size: u64 },
    #[error("{} is empty", .0.display())]
    Empty(PathBuf),
    #[error("{action} {}: {source}", path.display())]
    Io {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Lower-cases and validates a username so it is a safe file name: ASCII letters, digits, `-`,
/// `_`, `.` and the `[`/`]` of bot accounts (`dependabot[bot]`); no separators, no leading dot.
pub fn validate_name(name: &str) -> Result<String, TagError> {
    let lower = name.trim().to_ascii_lowercase();
    let valid = !lower.is_empty()
        && lower.len() <= 100
        && !lower.starts_with('.')
        && lower
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '[' | ']'));
    if valid {
        Ok(lower)
    } else {
        Err(TagError::InvalidName(name.to_owned()))
    }
}

/// Whose tag a file is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagOwner {
    /// `default.<ext>`: your tag, and the fallback for everyone without one.
    Default,
    /// `<platform>/<name>.<ext>`, or `<name>.<ext>` for any platform.
    Author {
        platform: Option<Platform>,
        name: String,
    },
}

impl TagOwner {
    /// Parses `github:alice`, `gitlab:jdoe` or `alice`.
    pub fn parse(spec: &str) -> Result<Self, TagError> {
        let (platform, name) = match spec.split_once(':') {
            Some((platform, name)) => (
                Some(
                    platform
                        .parse::<Platform>()
                        .map_err(|_| TagError::InvalidOwner(spec.to_owned()))?,
                ),
                name,
            ),
            None => (None, spec),
        };
        let name = validate_name(name)?;
        if platform.is_none() && name == DEFAULT_STEM {
            return Err(TagError::InvalidOwner(spec.to_owned()));
        }
        Ok(Self::Author { platform, name })
    }

    /// Path without extension, inside `tags_dir`.
    fn stem(&self, tags_dir: &Path) -> PathBuf {
        match self {
            Self::Default => tags_dir.join(DEFAULT_STEM),
            Self::Author {
                platform: Some(p),
                name,
            } => tags_dir.join(p.as_str()).join(name),
            Self::Author {
                platform: None,
                name,
            } => tags_dir.join(name),
        }
    }
}

impl fmt::Display for TagOwner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Default => f.write_str("default (you, and everyone without a tag)"),
            Self::Author {
                platform: Some(p),
                name,
            } => write!(f, "{p}:{name}"),
            Self::Author {
                platform: None,
                name,
            } => write!(f, "{name} (any platform)"),
        }
    }
}

/// The existing tag file for `owner`, trying each extension in order.
pub fn find(tags_dir: &Path, owner: &TagOwner) -> Option<PathBuf> {
    let stem = owner.stem(tags_dir);
    EXTENSIONS
        .iter()
        .map(|ext| stem.with_extension(ext))
        .find(|path| path.is_file())
}

/// Tag for a merge by `author` on `platform` (`None`: any platform): the platform-specific
/// file, then the any-platform file, then the default. An author name that isn't a safe file
/// name gets the default tag.
pub fn lookup(tags_dir: &Path, platform: Option<Platform>, author: &str) -> Option<PathBuf> {
    if let Ok(name) = validate_name(author) {
        let mut candidates = Vec::with_capacity(2);
        if platform.is_some() {
            candidates.push(TagOwner::Author {
                platform,
                name: name.clone(),
            });
        }
        candidates.push(TagOwner::Author {
            platform: None,
            name,
        });
        if let Some(path) = candidates.iter().find_map(|owner| find(tags_dir, owner)) {
            return Some(path);
        }
    }
    find(tags_dir, &TagOwner::Default)
}

/// Copies `source` into `tags_dir` as `owner`'s tag, replacing any file of theirs with another
/// extension. Returns the new path.
pub fn set(tags_dir: &Path, source: &Path, owner: &TagOwner) -> Result<PathBuf, TagError> {
    let ext = source
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .filter(|e| EXTENSIONS.contains(&e.as_str()))
        .ok_or_else(|| TagError::UnsupportedFormat(source.display().to_string()))?;
    let io = |action, path: &Path| {
        let path = path.to_owned();
        move |source| TagError::Io {
            action,
            path,
            source,
        }
    };
    let meta = fs::metadata(source).map_err(io("reading", source))?;
    if !meta.is_file() {
        return Err(TagError::UnsupportedFormat(source.display().to_string()));
    }
    if meta.len() == 0 {
        return Err(TagError::Empty(source.to_owned()));
    }
    if meta.len() > MAX_TAG_BYTES {
        return Err(TagError::TooLarge {
            path: source.to_owned(),
            size: meta.len(),
        });
    }

    let stem = owner.stem(tags_dir);
    let dest = stem.with_extension(&ext);
    let parent = dest.parent().unwrap_or(tags_dir);
    fs::create_dir_all(parent).map_err(io("creating", parent))?;
    // Copy to a temp name first so a failed copy leaves the old tag in place.
    let tmp = stem.with_extension(format!("{ext}.tmp"));
    fs::copy(source, &tmp).map_err(io("copying to", &tmp))?;
    for other in EXTENSIONS.iter().filter(|e| **e != ext) {
        let old = stem.with_extension(other);
        if old.is_file() {
            fs::remove_file(&old).map_err(io("removing", &old))?;
        }
    }
    fs::rename(&tmp, &dest).map_err(io("writing", &dest))?;
    Ok(dest)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagEntry {
    pub owner: TagOwner,
    pub path: PathBuf,
}

/// Every tag file in `tags_dir`: default, any-platform and per-platform tags.
pub fn list(tags_dir: &Path) -> Result<Vec<TagEntry>, TagError> {
    let mut entries = Vec::new();
    collect(tags_dir, None, &mut entries)?;
    for platform in Platform::ALL {
        let dir = tags_dir.join(platform.as_str());
        if dir.is_dir() {
            collect(&dir, Some(platform), &mut entries)?;
        }
    }
    entries.sort_by(|a, b| {
        (a.owner != TagOwner::Default, &a.path).cmp(&(b.owner != TagOwner::Default, &b.path))
    });
    Ok(entries)
}

fn collect(
    dir: &Path,
    platform: Option<Platform>,
    entries: &mut Vec<TagEntry>,
) -> Result<(), TagError> {
    let read = fs::read_dir(dir).map_err(|source| TagError::Io {
        action: "reading",
        path: dir.to_owned(),
        source,
    })?;
    for item in read.flatten() {
        let path = item.path();
        if !path.is_file() {
            continue;
        }
        let (Some(stem), Some(ext)) = (
            path.file_stem().and_then(|s| s.to_str()),
            path.extension().and_then(|e| e.to_str()),
        ) else {
            continue;
        };
        if !EXTENSIONS.contains(&ext) {
            continue;
        }
        let owner = if platform.is_none() && stem == DEFAULT_STEM {
            TagOwner::Default
        } else if let Ok(name) = validate_name(stem) {
            TagOwner::Author { platform, name }
        } else {
            continue;
        };
        entries.push(TagEntry { owner, path });
    }
    Ok(())
}

/// `path` relative to `tags_dir` for logs and state, e.g. `github/alice.wav`.
pub fn display_name(tags_dir: &Path, path: &Path) -> String {
    path.strip_prefix(tags_dir)
        .unwrap_or(path)
        .display()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(dir: &Path, rel: &str) -> PathBuf {
        let path = dir.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"RIFF").unwrap();
        path
    }

    #[test]
    fn name_validation() {
        assert_eq!(validate_name("OctoCat").unwrap(), "octocat");
        assert_eq!(validate_name("bob.smith").unwrap(), "bob.smith");
        assert_eq!(validate_name("dependabot[bot]").unwrap(), "dependabot[bot]");
        for bad in [
            "",
            "..",
            ".hidden",
            "a/b",
            "a\\b",
            "../etc/passwd",
            "a\0b",
            "a b",
            "ñ",
        ] {
            assert!(validate_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn owner_parsing() {
        assert_eq!(
            TagOwner::parse("github:Alice").unwrap(),
            TagOwner::Author {
                platform: Some(Platform::GitHub),
                name: "alice".into()
            }
        );
        assert_eq!(
            TagOwner::parse("alice").unwrap(),
            TagOwner::Author {
                platform: None,
                name: "alice".into()
            }
        );
        assert!(TagOwner::parse("bitbucket:alice").is_err());
        assert!(TagOwner::parse("github:../x").is_err());
        assert!(TagOwner::parse("default").is_err());
        assert!(TagOwner::parse("github:").is_err());
    }

    #[test]
    fn lookup_order() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        assert_eq!(lookup(d, Some(Platform::GitHub), "alice"), None);

        let default = touch(d, "default.ogg");
        assert_eq!(
            lookup(d, Some(Platform::GitHub), "alice"),
            Some(default.clone())
        );

        let any = touch(d, "alice.mp3");
        assert_eq!(
            lookup(d, Some(Platform::GitHub), "Alice"),
            Some(any.clone())
        );
        assert_eq!(lookup(d, None, "alice"), Some(any.clone()));

        let gh = touch(d, "github/alice.wav");
        assert_eq!(lookup(d, Some(Platform::GitHub), "ALICE"), Some(gh));
        assert_eq!(lookup(d, Some(Platform::GitLab), "alice"), Some(any));
        assert_eq!(lookup(d, Some(Platform::GitLab), "bob"), Some(default));
    }

    #[test]
    fn extension_order() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "default.mp3");
        let wav = touch(dir.path(), "default.wav");
        assert_eq!(find(dir.path(), &TagOwner::Default), Some(wav));
    }

    #[test]
    fn unsafe_author_names_never_escape() {
        let root = tempfile::tempdir().unwrap();
        let tags = root.path().join("tags");
        let default = touch(&tags, "default.wav");
        // Files outside TAGS_DIR that a traversal would hit.
        touch(root.path(), "evil.wav");
        touch(root.path(), "github/evil.wav");
        for author in [
            "../evil",
            "..",
            "../github/evil",
            "/etc/passwd",
            "a/../../evil",
        ] {
            assert_eq!(
                lookup(&tags, Some(Platform::GitHub), author),
                Some(default.clone()),
                "{author}"
            );
        }
    }

    #[test]
    fn set_replaces_other_extensions() {
        let src_dir = tempfile::tempdir().unwrap();
        let tags = tempfile::tempdir().unwrap();
        let owner = TagOwner::parse("github:alice").unwrap();
        let old = touch(tags.path(), "github/alice.ogg");

        let src = touch(src_dir.path(), "My Tag.WAV");
        let dest = set(tags.path(), &src, &owner).unwrap();
        assert_eq!(dest, tags.path().join("github/alice.wav"));
        assert!(dest.is_file());
        assert!(!old.exists());
        assert_eq!(find(tags.path(), &owner), Some(dest));
        assert!(!tags.path().join("github/alice.wav.tmp").exists());

        let default = set(tags.path(), &src, &TagOwner::Default).unwrap();
        assert_eq!(default, tags.path().join("default.wav"));
    }

    #[test]
    fn set_validates_the_file() {
        let src_dir = tempfile::tempdir().unwrap();
        let tags = tempfile::tempdir().unwrap();
        let txt = touch(src_dir.path(), "notes.txt");
        assert!(matches!(
            set(tags.path(), &txt, &TagOwner::Default),
            Err(TagError::UnsupportedFormat(_))
        ));

        let empty = src_dir.path().join("empty.wav");
        fs::write(&empty, b"").unwrap();
        assert!(matches!(
            set(tags.path(), &empty, &TagOwner::Default),
            Err(TagError::Empty(_))
        ));

        let big = src_dir.path().join("big.wav");
        fs::write(&big, vec![0u8; (MAX_TAG_BYTES + 1) as usize]).unwrap();
        assert!(matches!(
            set(tags.path(), &big, &TagOwner::Default),
            Err(TagError::TooLarge { .. })
        ));

        let missing = src_dir.path().join("missing.wav");
        assert!(matches!(
            set(tags.path(), &missing, &TagOwner::Default),
            Err(TagError::Io { .. })
        ));
    }

    #[test]
    fn lists_tags() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        touch(d, "default.wav");
        touch(d, "alice.mp3");
        touch(d, "github/octocat.ogg");
        touch(d, "gitlab/jdoe.flac");
        touch(d, "README.md");
        touch(d, "github/.hidden.wav");

        let entries = list(d).unwrap();
        let names: Vec<String> = entries.iter().map(|e| e.owner.to_string()).collect();
        assert_eq!(
            names,
            [
                "default (you, and everyone without a tag)",
                "alice (any platform)",
                "github:octocat",
                "gitlab:jdoe"
            ]
        );
        assert_eq!(display_name(d, &entries[2].path), "github/octocat.ogg");
    }
}
