use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use thiserror::Error;
use walkdir::WalkDir;

const SUPPORTED_LANGUAGES_FILE: &str = "supported_languages.txt";
const USER_CONFIG_FILE: &str = "configs.user.ini";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GameLanguageOptions {
    pub languages: Vec<String>,
    pub current_language: Option<String>,
    pub supported_path: PathBuf,
    pub config_path: PathBuf,
}

pub fn read_language_options(
    game_directory: &Path,
) -> Result<Option<GameLanguageOptions>, GameLanguageError> {
    if !game_directory.is_dir() {
        return Ok(None);
    }
    let Some(supported_path) = find_supported_languages(game_directory) else {
        return Ok(None);
    };
    let text = fs::read_to_string(&supported_path).map_err(|source| GameLanguageError::Read {
        path: supported_path.clone(),
        source,
    })?;
    let languages: BTreeSet<_> = text
        .split(['\r', '\n', ',', ';'])
        .filter_map(normalize_language)
        .collect();
    if languages.is_empty() {
        return Ok(None);
    }

    let directory = supported_path
        .parent()
        .ok_or_else(|| GameLanguageError::InvalidPath(supported_path.clone()))?;
    let config_path = directory.join(USER_CONFIG_FILE);
    let current_language = if config_path.is_file() {
        fs::read_to_string(&config_path)
            .map_err(|source| GameLanguageError::Read {
                path: config_path.clone(),
                source,
            })?
            .lines()
            .find_map(parse_language_line)
    } else {
        None
    };

    Ok(Some(GameLanguageOptions {
        languages: languages.into_iter().collect(),
        current_language,
        supported_path,
        config_path,
    }))
}

pub fn apply_language(game_directory: &Path, language: &str) -> Result<String, GameLanguageError> {
    let options = read_language_options(game_directory)?.ok_or(GameLanguageError::NotSupported)?;
    let normalized =
        normalize_language(language).ok_or_else(|| GameLanguageError::InvalidLanguage(language.into()))?;
    if !options
        .languages
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(&normalized))
    {
        return Err(GameLanguageError::UnavailableLanguage(normalized));
    }

    let existing = if options.config_path.is_file() {
        fs::read_to_string(&options.config_path).map_err(|source| GameLanguageError::Read {
            path: options.config_path.clone(),
            source,
        })?
    } else {
        String::new()
    };
    let mut output = Vec::new();
    let mut replaced = false;
    for line in existing.lines() {
        if parse_language_line(line).is_none() {
            output.push(line.to_owned());
        } else if !replaced {
            output.push(format!("language={normalized}"));
            replaced = true;
        }
    }
    if !replaced {
        output.push(format!("language={normalized}"));
    }
    let mut bytes = output.join("\n").into_bytes();
    bytes.push(b'\n');

    write_with_rollback(&options.config_path, &bytes)?;
    let verified = fs::read_to_string(&options.config_path).map_err(|source| GameLanguageError::Read {
        path: options.config_path.clone(),
        source,
    })?;
    if !verified
        .lines()
        .any(|line| line == format!("language={normalized}"))
    {
        return Err(GameLanguageError::Verification(options.config_path));
    }
    Ok(normalized)
}

fn find_supported_languages(game_directory: &Path) -> Option<PathBuf> {
    let direct = game_directory.join(SUPPORTED_LANGUAGES_FILE);
    if direct.is_file() {
        return Some(direct);
    }
    let steam_settings = game_directory
        .join("steam_settings")
        .join(SUPPORTED_LANGUAGES_FILE);
    if steam_settings.is_file() {
        return Some(steam_settings);
    }

    let mut candidates: Vec<_> = WalkDir::new(game_directory)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case(SUPPORTED_LANGUAGES_FILE)
        })
        .map(|entry| entry.into_path())
        .collect();
    candidates.sort_by(|left, right| {
        left.components()
            .count()
            .cmp(&right.components().count())
            .then_with(|| {
                left.to_string_lossy()
                    .to_lowercase()
                    .cmp(&right.to_string_lossy().to_lowercase())
            })
    });
    candidates.into_iter().next()
}

fn normalize_language(value: &str) -> Option<String> {
    let normalized = value.trim().to_ascii_lowercase();
    (!normalized.is_empty()
        && normalized.len() <= 64
        && normalized
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')))
    .then_some(normalized)
}

fn parse_language_line(line: &str) -> Option<String> {
    let (key, value) = line.split_once('=')?;
    key.trim()
        .eq_ignore_ascii_case("language")
        .then(|| normalize_language(value))
        .flatten()
}

fn write_with_rollback(path: &Path, bytes: &[u8]) -> Result<(), GameLanguageError> {
    let parent = path
        .parent()
        .ok_or_else(|| GameLanguageError::InvalidPath(path.to_path_buf()))?;
    fs::create_dir_all(parent).map_err(|source| GameLanguageError::Write {
        path: parent.to_path_buf(),
        source,
    })?;
    let temporary = path.with_extension("ini.drydock-new");
    let backup = path.with_extension("ini.drydock-backup");

    let mut output = fs::File::create(&temporary).map_err(|source| GameLanguageError::Write {
        path: temporary.clone(),
        source,
    })?;
    output
        .write_all(bytes)
        .map_err(|source| GameLanguageError::Write {
            path: temporary.clone(),
            source,
        })?;
    output.sync_all().map_err(|source| GameLanguageError::Write {
        path: temporary.clone(),
        source,
    })?;

    if backup.exists() {
        let _ = fs::remove_file(&backup);
    }
    let had_original = path.exists();
    if had_original {
        fs::rename(path, &backup).map_err(|source| GameLanguageError::Write {
            path: path.to_path_buf(),
            source,
        })?;
    }
    if let Err(source) = fs::rename(&temporary, path) {
        if had_original {
            let _ = fs::rename(&backup, path);
        }
        return Err(GameLanguageError::Write {
            path: path.to_path_buf(),
            source,
        });
    }
    if had_original {
        let _ = fs::remove_file(backup);
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum GameLanguageError {
    #[error("supported_languages.txt is missing or contains no supported languages")]
    NotSupported,
    #[error("invalid language: {0}")]
    InvalidLanguage(String),
    #[error("the selected language is not supported: {0}")]
    UnavailableLanguage(String),
    #[error("cannot read language file {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot write language file {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid language file path: {0}")]
    InvalidPath(PathBuf),
    #[error("language setting verification failed at {0}")]
    Verification(PathBuf),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_nested_language_files_and_replaces_duplicates() {
        let directory = tempfile::tempdir().expect("tempdir");
        let nested = directory.path().join("bin").join("steam_settings");
        fs::create_dir_all(&nested).expect("create nested path");
        fs::write(
            nested.join(SUPPORTED_LANGUAGES_FILE),
            "English, german\nJapanese;german",
        )
        .expect("write supported languages");
        fs::write(
            nested.join(USER_CONFIG_FILE),
            "foo=bar\nlanguage=english\nLANGUAGE=japanese\n",
        )
        .expect("write config");

        let options = read_language_options(directory.path())
            .expect("read options")
            .expect("language options");
        assert_eq!(options.languages, vec!["english", "german", "japanese"]);
        assert_eq!(options.current_language.as_deref(), Some("english"));

        apply_language(directory.path(), " German ").expect("apply language");
        let output = fs::read_to_string(nested.join(USER_CONFIG_FILE)).expect("read config");
        assert_eq!(output, "foo=bar\nlanguage=german\n");
    }

    #[test]
    fn rejects_unlisted_language() {
        let directory = tempfile::tempdir().expect("tempdir");
        fs::write(directory.path().join(SUPPORTED_LANGUAGES_FILE), "english")
            .expect("write supported languages");
        let error = apply_language(directory.path(), "german").expect_err("reject language");
        assert!(matches!(error, GameLanguageError::UnavailableLanguage(_)));
    }
}
