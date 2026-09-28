use std::fs;
use std::path::{Path, PathBuf};

const HARNESS_TYPES: [&str; 3] = ["HttpClientHarness", "LoopbackReply", "LoopbackServer"];

#[test]
fn normal_dependencies_do_not_enable_http_test_support() {
    let root = workspace_root();
    for manifest in crate_manifests(&root) {
        let source = read(&manifest);
        for (section, body) in manifest_sections(&source) {
            if !is_normal_dependency_section(&section) {
                continue;
            }

            let enables_feature = if section == "dependencies" || section.ends_with(".dependencies")
            {
                dependency_entries(&body)
                    .iter()
                    .any(|entry| is_http_dependency(entry) && enables_test_support(entry))
            } else {
                (section_dependency_name(&section) == Some("iaam-http")
                    || names_http_package(&body))
                    && enables_test_support(&body)
            };

            assert!(
                !enables_feature,
                "{} enables iaam-http/test-support in normal dependency section [{section}]",
                manifest.display()
            );
        }
    }
}

#[test]
fn production_sources_do_not_name_the_http_harness() {
    let root = workspace_root();
    let harness = root.join("crates/iaam-http/src/test_support.rs");

    for manifest in crate_manifests(&root) {
        let crate_root = manifest
            .parent()
            .unwrap_or_else(|| panic!("{} has no crate directory", manifest.display()));
        for source_path in rust_sources(&crate_root.join("src")) {
            if source_path == harness {
                continue;
            }
            let source = read(&source_path);
            for harness_type in HARNESS_TYPES {
                assert!(
                    !source.contains(harness_type),
                    "production source {} names test harness type {harness_type}",
                    source_path.display()
                );
            }
        }
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap_or_else(|| panic!("iaam-http manifest is not beneath the workspace root"))
        .to_path_buf()
}

fn crate_manifests(root: &Path) -> Vec<PathBuf> {
    let crates = root.join("crates");
    let mut manifests = fs::read_dir(&crates)
        .unwrap_or_else(|error| panic!("read {}: {error}", crates.display()))
        .filter_map(|entry| {
            let entry = entry.unwrap_or_else(|error| panic!("read crate entry: {error}"));
            let manifest = entry.path().join("Cargo.toml");
            manifest.is_file().then_some(manifest)
        })
        .collect::<Vec<_>>();
    manifests.sort();
    manifests
}

fn manifest_sections(manifest: &str) -> Vec<(String, String)> {
    let mut sections = Vec::new();
    let mut name = None::<String>;
    let mut body = String::new();

    for line in manifest.lines() {
        let trimmed = line.trim();
        if let Some(section) = trimmed
            .strip_prefix('[')
            .and_then(|value| value.strip_suffix(']'))
        {
            if let Some(previous) = name.replace(section.to_owned()) {
                sections.push((previous, std::mem::take(&mut body)));
            }
        } else if name.is_some() {
            body.push_str(line);
            body.push('\n');
        }
    }
    if let Some(name) = name {
        sections.push((name, body));
    }
    sections
}

fn is_normal_dependency_section(section: &str) -> bool {
    !section.contains("dev-dependencies")
        && !section.contains("build-dependencies")
        && (section == "dependencies"
            || section.starts_with("dependencies.")
            || section.contains(".dependencies"))
}

fn dependency_entries(section: &str) -> Vec<String> {
    let mut entries = Vec::new();
    let mut current = String::new();

    for line in section.lines() {
        let starts_entry = !line.starts_with(char::is_whitespace)
            && !line.trim_start().starts_with('#')
            && line.contains('=');
        if starts_entry && !current.is_empty() {
            entries.push(std::mem::take(&mut current));
        }
        if starts_entry || !current.is_empty() {
            current.push_str(line);
            current.push('\n');
        }
    }
    if !current.is_empty() {
        entries.push(current);
    }
    entries
}

fn is_http_dependency(entry: &str) -> bool {
    let key = entry
        .split_once('=')
        .map(|(key, _)| key.trim().trim_matches(['"', '\'']))
        .unwrap_or_default();
    key == "iaam-http" || names_http_package(entry)
}

fn names_http_package(text: &str) -> bool {
    compact(text).contains("package=\"iaam-http\"") || compact(text).contains("package='iaam-http'")
}

fn enables_test_support(text: &str) -> bool {
    text.contains("\"test-support\"") || text.contains("'test-support'")
}

fn section_dependency_name(section: &str) -> Option<&str> {
    section
        .rsplit('.')
        .next()
        .map(|name| name.trim().trim_matches(['"', '\'']))
}

fn compact(text: &str) -> String {
    text.chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}

fn rust_sources(root: &Path) -> Vec<PathBuf> {
    let mut pending = vec![root.to_path_buf()];
    let mut sources = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)
            .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()))
        {
            let path = entry
                .unwrap_or_else(|error| panic!("read source entry: {error}"))
                .path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                sources.push(path);
            }
        }
    }
    sources.sort();
    sources
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}
