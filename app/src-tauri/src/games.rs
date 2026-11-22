//! The game library: AWCC's own library file (it already found your games), read-only.

use serde::Serialize;
use serde_json::Value;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Game {
    pub id: String,
    pub title: String,
    pub provider: String,
    pub exe: Option<String>,
    pub genre: Option<String>,
}

pub fn awcc_library() -> PathBuf {
    PathBuf::from(r"C:\ProgramData\Alienware\Alienware Command Center\GameLibrary\GameLibrary.json")
}

pub fn parse(text: &str) -> Vec<Game> {
    let v: Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).unwrap_or(Value::Null);
    let mut games: Vec<Game> = v["Games"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|g| {
            let title = g["DisplayTitle"].as_str().filter(|s| !s.is_empty()).or(g["Title"].as_str())?.to_string();
            Some(Game { id: g["ID"].as_str().unwrap_or(&title).to_string(), title, provider: g["ProviderName"].as_str().unwrap_or("").to_string(), exe: g["ExePath"].as_str().filter(|s| !s.is_empty()).map(String::from), genre: g["Genre"].as_str().map(String::from) })
        })
        .collect();
    games.sort_by_key(|g| g.title.to_lowercase());
    games
}

pub fn read() -> Vec<Game> {
    std::fs::read_to_string(awcc_library()).map(|t| parse(&t)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #[test]
    fn parses_awcc_library_entries() {
        let g = super::parse("\u{feff}{\"Games\":[{\"ID\":\"b\",\"Title\":\"Warframe\",\"ProviderName\":\"Steam\",\"ExePath\":\"C:/w.exe\"},{\"ID\":\"a\",\"Title\":\"Cities: Skylines\",\"DisplayTitle\":\"\",\"ProviderName\":\"Steam\"},{\"Bad\":1}]}");
        assert_eq!(g.iter().map(|x| x.title.as_str()).collect::<Vec<_>>(), vec!["Cities: Skylines", "Warframe"]);
        assert_eq!(g[1].exe.as_deref(), Some("C:/w.exe"));
    }
}

/// Reads the real AWCC game library on this machine. Run with `-- --ignored`.
#[cfg(test)]
#[test]
#[ignore]
fn reads_this_machines_game_library() {
    let g = read();
    eprintln!("{:?}", g.iter().map(|x| (&x.title, &x.provider)).collect::<Vec<_>>());
    assert!(!g.is_empty());
}
