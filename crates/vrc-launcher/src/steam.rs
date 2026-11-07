//! Steam library discovery for the VRChat install.
//!
//! Reads `steamapps/libraryfolders.vdf` and `steamapps/appmanifest_250820.acf`
//! with a tolerant parser: malformed or unexpected content yields fewer
//! candidates, never an error and never a panic. Only the filesystem is
//! touched — no process inspection (RULES.md §5).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Steam's app id for VRChat.
const VRCHAT_APP_ID: &str = "250820";
/// Executable name inside the install dir.
const VRCHAT_EXE: &str = "VRChat.exe";
/// Directory under `steamapps/` that holds game installs.
const COMMON_DIR: &str = "common";

/// A parsed VDF node: either a string value or an ordered section of
/// key/value pairs. Sections may nest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum VdfNode {
    Value(String),
    Section(Vec<(String, VdfNode)>),
}

impl VdfNode {
    fn get(&self, key: &str) -> Option<&VdfNode> {
        match self {
            VdfNode::Section(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            VdfNode::Value(_) => None,
        }
    }

    fn as_str(&self) -> Option<&str> {
        match self {
            VdfNode::Value(s) => Some(s),
            VdfNode::Section(_) => None,
        }
    }

    fn as_section(&self) -> Option<&[(String, VdfNode)]> {
        match self {
            VdfNode::Section(pairs) => Some(pairs),
            VdfNode::Value(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Str(String),
    Open,
    Close,
}

/// Tokenize VDF text: `"quoted"` strings (with `\\`/`\"` escapes), `{`, `}`.
/// Unquoted runs are accepted as bare strings for tolerance. Malformed input
/// simply produces fewer tokens.
fn tokenize(input: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut chars = input.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            c if c.is_whitespace() => {
                chars.next();
            }
            '{' => {
                chars.next();
                tokens.push(Token::Open);
            }
            '}' => {
                chars.next();
                tokens.push(Token::Close);
            }
            '"' => {
                chars.next();
                let mut value = String::new();
                while let Some(ch) = chars.next() {
                    match ch {
                        '"' => break,
                        '\\' => match chars.next() {
                            Some('n') => value.push('\n'),
                            Some(other) => value.push(other),
                            None => break,
                        },
                        other => value.push(other),
                    }
                }
                tokens.push(Token::Str(value));
            }
            _ => {
                let mut value = String::new();
                while let Some(&ch) = chars.peek() {
                    if ch.is_whitespace() || ch == '{' || ch == '}' || ch == '"' {
                        break;
                    }
                    value.push(ch);
                    chars.next();
                }
                if !value.is_empty() {
                    tokens.push(Token::Str(value));
                } else {
                    chars.next();
                }
            }
        }
    }
    tokens
}

/// Deepest section nesting the parser accepts — real VDF nests ~4
/// levels; a pathological file must not overflow the stack (the parser
/// promises "never a panic").
const MAX_DEPTH: usize = 64;

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
    depth: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&'a Token> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<&'a Token> {
        let token = self.tokens.get(self.pos);
        self.pos += 1;
        token
    }

    /// Parse `key value` pairs until `}` (or EOF). Returns `None` when the
    /// stream does not start a section body or nesting exceeds
    /// [`MAX_DEPTH`].
    fn section(&mut self) -> Option<Vec<(String, VdfNode)>> {
        if self.depth >= MAX_DEPTH {
            return None;
        }
        self.depth += 1;
        let out = self.section_inner();
        self.depth -= 1;
        out
    }

    fn section_inner(&mut self) -> Option<Vec<(String, VdfNode)>> {
        let mut pairs = Vec::new();
        loop {
            match self.peek() {
                Some(Token::Close) | None => {
                    if matches!(self.peek(), Some(Token::Close)) {
                        self.pos += 1;
                    }
                    return Some(pairs);
                }
                Some(Token::Str(key)) => {
                    let key = key.clone();
                    self.pos += 1;
                    let value = self.value()?;
                    pairs.push((key, value));
                }
                Some(Token::Open) => return None,
            }
        }
    }

    fn value(&mut self) -> Option<VdfNode> {
        match self.next()? {
            Token::Open => self.section().map(VdfNode::Section),
            Token::Str(s) => Some(VdfNode::Value(s.clone())),
            Token::Close => None,
        }
    }
}

/// Parse VDF text into a root [`VdfNode::Section`]. Returns `None` for
/// empty or structurally unusable input.
pub(crate) fn parse_vdf(input: &str) -> Option<VdfNode> {
    let tokens = tokenize(input);
    let mut parser = Parser {
        tokens: &tokens,
        pos: 0,
        depth: 0,
    };
    // A document is `{ pairs }`; tolerate a missing outer brace by parsing
    // bare pairs too.
    if matches!(parser.peek(), Some(Token::Open)) {
        parser.pos += 1;
    }
    parser.section().map(VdfNode::Section)
}

/// Steam install roots to probe, per platform.
fn steam_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    #[cfg(windows)]
    {
        let mut push_env = |names: &[&str]| {
            for name in names {
                if let Some(value) = std::env::var_os(name) {
                    if !value.is_empty() {
                        roots.push(PathBuf::from(value));
                    }
                }
            }
        };
        push_env(&["ProgramFiles(x86)", "ProgramFiles", "ProgramW6432"]);
        roots.push(PathBuf::from(r"C:\Program Files (x86)\Steam"));
        roots.push(PathBuf::from(r"C:\Program Files\Steam"));
    }
    #[cfg(not(windows))]
    {
        if let Some(home) = std::env::var_os("HOME") {
            let home = PathBuf::from(home);
            roots.push(home.join(".steam/steam"));
            roots.push(home.join(".local/share/Steam"));
        }
    }
    roots
}

/// Parse `libraryfolders.vdf` content and return every library root path it
/// lists. Tolerant: unreadable structure yields an empty list.
fn library_paths_from_vdf(input: &str) -> Vec<PathBuf> {
    let Some(root) = parse_vdf(input) else {
        return Vec::new();
    };
    let Some(folders) = root.get("libraryfolders").and_then(VdfNode::as_section) else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    for (_, entry) in folders {
        if let Some(path) = entry.get("path").and_then(VdfNode::as_str) {
            if !path.is_empty() {
                paths.push(PathBuf::from(path));
            }
        }
    }
    paths
}

/// `path` values listed in `<steam_root>/steamapps/libraryfolders.vdf`.
fn steam_library_roots(steam_root: &Path) -> Vec<PathBuf> {
    let vdf = steam_root.join("steamapps").join("libraryfolders.vdf");
    let Ok(text) = std::fs::read_to_string(&vdf) else {
        return Vec::new();
    };
    library_paths_from_vdf(&text)
}

/// `installdir` from `steamapps/appmanifest_<appid>.acf`, when present.
fn appmanifest_installdir(library_root: &Path, app_id: &str) -> Option<String> {
    let acf = library_root
        .join("steamapps")
        .join(format!("appmanifest_{app_id}.acf"));
    let text = std::fs::read_to_string(acf).ok()?;
    let root = parse_vdf(&text)?;
    // Real appmanifest files nest fields under the top-level "AppState"
    // key; accept a flat document too for tolerance.
    let body = root.get("AppState").unwrap_or(&root);
    body.get("installdir")
        .and_then(VdfNode::as_str)
        .map(str::to_string)
}

/// Candidate VRChat install directories inside one Steam library root:
/// the `installdir` from the appmanifest when present, else the conventional
/// `steamapps/common/VRChat`. Both are existence-checked.
fn vrchat_dirs_in_library(library_root: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(installdir) = appmanifest_installdir(library_root, VRCHAT_APP_ID) {
        if !installdir.is_empty()
            && !installdir.contains(['/', '\\'])
            && installdir != "."
            && installdir != ".."
        {
            dirs.push(
                library_root
                    .join("steamapps")
                    .join(COMMON_DIR)
                    .join(&installdir),
            );
        }
    }
    dirs.push(
        library_root
            .join("steamapps")
            .join(COMMON_DIR)
            .join("VRChat"),
    );
    dirs
}

/// `true` when `dir` looks like a VRChat install (contains `VRChat.exe`).
pub(crate) fn is_vrchat_install(dir: &Path) -> bool {
    dir.join(VRCHAT_EXE).is_file()
}

/// Locate the VRChat install directory by scanning Steam libraries.
///
/// Probes every Steam root, every library listed in `libraryfolders.vdf`
/// (plus the Steam root itself, which is the implicit first library), and
/// returns the first directory containing `VRChat.exe`. Returns `None`
/// when Steam or VRChat is not installed.
pub fn find_vrchat() -> Option<PathBuf> {
    let mut seen = BTreeSet::new();
    for root in steam_roots() {
        let mut candidates = vec![root.clone()];
        candidates.extend(steam_library_roots(&root));
        for library in candidates {
            // Deduplicate by canonical-ish string form; cheap and avoids
            // rescanning the same library listed twice.
            let key = library.to_string_lossy().to_lowercase();
            if !seen.insert(key) {
                continue;
            }
            for dir in vrchat_dirs_in_library(&library) {
                if is_vrchat_install(&dir) {
                    return Some(dir);
                }
            }
        }
    }
    None
}

/// Library paths parsed from VDF text — exposed for tests.
#[cfg(test)]
pub(crate) fn library_paths(input: &str) -> Vec<PathBuf> {
    library_paths_from_vdf(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    const VDF: &str = r#"
"libraryfolders"
{
    "0"
    {
        "path"        "C:\\Program Files (x86)\\Steam"
        "label"       ""
        "contentid"   "123456789"
        "apps"
        {
            "4000"     "123"
            "250820"   "456"
        }
    }
    "1"
    {
        "path"        "D:\\SteamLibrary"
        "label"       "Games"
        "apps"
        {
            "250820"   "789"
        }
    }
}
"#;

    #[test]
    fn parses_libraryfolders_vdf() {
        let paths = library_paths(VDF);
        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0], PathBuf::from(r"C:\Program Files (x86)\Steam"));
        assert_eq!(paths[1], PathBuf::from(r"D:\SteamLibrary"));
    }

    #[test]
    fn malformed_vdf_yields_nothing() {
        for bad in [
            "",
            "not vdf at all",
            "\"libraryfolders\" {",
            "{}{}",
            "\"a\"",
        ] {
            assert!(library_paths(bad).is_empty(), "{bad:?}");
        }
    }

    #[test]
    fn ignores_non_path_entries() {
        let vdf = r#""libraryfolders" { "0" { "label" "x" } "1" { "path" "E:\\Lib" } }"#;
        assert_eq!(library_paths(vdf), vec![PathBuf::from(r"E:\Lib")]);
    }

    #[test]
    fn appmanifest_installdir_parses() {
        // Synthetic ACF; written to a temp dir so no real Steam data is used.
        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path();
        std::fs::create_dir_all(lib.join("steamapps")).unwrap();
        std::fs::write(
            lib.join("steamapps").join("appmanifest_250820.acf"),
            r#""AppState"
{
    "appid"         "250820"
    "installdir"    "VRChat"
}
"#,
        )
        .unwrap();
        assert_eq!(
            appmanifest_installdir(lib, VRCHAT_APP_ID).as_deref(),
            Some("VRChat")
        );
        assert_eq!(appmanifest_installdir(lib, "999999"), None);
    }

    #[test]
    fn deeply_nested_vdf_does_not_overflow() {
        // "k" { repeated nests a section per pair; the depth cap must
        // reject it cleanly instead of recursing until the stack dies.
        let mut input = String::new();
        for _ in 0..500 {
            input.push_str("\"k\" {");
        }
        assert!(parse_vdf(&input).is_none());
    }

    #[test]
    fn appmanifest_path_traversal_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path();
        std::fs::create_dir_all(lib.join("steamapps")).unwrap();
        for evil in ["..", "a/b", "a\\\\b", "."] {
            std::fs::write(
                lib.join("steamapps").join("appmanifest_250820.acf"),
                format!("\"installdir\" \"{evil}\""),
            )
            .unwrap();
            let dirs = vrchat_dirs_in_library(lib);
            assert!(
                dirs.iter().all(|d| d.ends_with("VRChat")),
                "installdir {evil:?} escaped common dir: {dirs:?}"
            );
        }
    }
}
