use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SteamUriAction {
    Install,
    Uninstall,
    Run,
}

pub fn open_steam_uri(app_id: u32, action: SteamUriAction) -> Result<(), SteamUriError> {
    if app_id == 0 {
        return Err(SteamUriError::InvalidAppId);
    }
    let uri = steam_uri(app_id, action);
    open::that(uri).map_err(SteamUriError::Open)
}

fn steam_uri(app_id: u32, action: SteamUriAction) -> String {
    let command = match action {
        SteamUriAction::Install => "install",
        SteamUriAction::Uninstall => "uninstall",
        SteamUriAction::Run => "run",
    };
    format!("steam://{command}/{app_id}")
}

#[derive(Debug, Error)]
pub enum SteamUriError {
    #[error("The selected app has no App ID")]
    InvalidAppId,
    #[error("Steam could not be opened: {0}")]
    Open(std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_only_numeric_steam_uris() {
        assert_eq!(
            steam_uri(111_300, SteamUriAction::Install),
            "steam://install/111300"
        );
        assert_eq!(
            steam_uri(111_300, SteamUriAction::Uninstall),
            "steam://uninstall/111300"
        );
        assert_eq!(steam_uri(111_300, SteamUriAction::Run), "steam://run/111300");
    }
}
