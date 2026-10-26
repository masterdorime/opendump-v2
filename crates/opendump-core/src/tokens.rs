//! `%LOCALAPPDATA%`-style token expansion.
//!
//! Tokens are resolved against a map so tests are deterministic and so the
//! desktop shell can inject Windows Known Folder paths (OneDrive redirects,
//! relocated Downloads) rather than assuming they sit under the profile.

use std::collections::HashMap;

pub type TokenMap = HashMap<String, String>;

/// The tokens the shipped rules use on a normal Windows machine.
pub fn default_tokens() -> TokenMap {
    let mut map = TokenMap::new();

    for (token, var) in [
        ("LOCALAPPDATA", "LOCALAPPDATA"),
        ("APPDATA", "APPDATA"),
        ("USERPROFILE", "USERPROFILE"),
        ("PROGRAMDATA", "PROGRAMDATA"),
        ("PUBLIC", "PUBLIC"),
        ("WINDIR", "WINDIR"),
        ("TEMP", "TEMP"),
        ("SYSTEMDRIVE", "SystemDrive"),
    ] {
        if let Ok(value) = std::env::var(var) {
            map.insert(token.to_string(), value);
        }
    }

    if let Ok(pf) = std::env::var("ProgramFiles") {
        map.insert("PROGRAMFILES".into(), pf);
    }
    if let Ok(pf86) = std::env::var("ProgramFiles(x86)") {
        map.insert("PROGRAMFILES(X86)".into(), pf86);
    }

    // Shell folders are not always under the profile; the desktop shell
    // overrides these via the Known Folder API.
    if let Ok(home) = std::env::var("USERPROFILE") {
        for (token, leaf) in [
            ("DOCUMENTS", "Documents"),
            ("DESKTOP", "Desktop"),
            ("DOWNLOADS", "Downloads"),
            ("PICTURES", "Pictures"),
            ("MUSIC", "Music"),
            ("VIDEOS", "Videos"),
        ] {
            map.insert(token.to_string(), format!("{home}\\{leaf}"));
        }
    }

    map
}

/// Expand every `%NAME%` token that exists in `tokens`. Unknown tokens are left
/// verbatim rather than silently emptied, so a typo is visible in the UI.
pub fn expand(input: &str, tokens: &TokenMap) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;

    while i < chars.len() {
        if chars[i] == '%' {
            if let Some(end) = next_percent(&chars, i + 1) {
                let name: String = chars[i + 1..end].iter().collect();
                if is_valid_name(&name) {
                    if let Some(value) = tokens.get(&name.to_ascii_uppercase()) {
                        out.push_str(value);
                        i = end + 1;
                        continue;
                    }
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }

    out
}

fn next_percent(chars: &[char], from: usize) -> Option<usize> {
    let limit = (from + 40).min(chars.len());
    (from..limit).find(|&i| chars[i] == '%')
}

fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '(' || c == ')')
}
