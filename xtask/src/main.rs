use std::{
    collections::BTreeMap,
    error::Error,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        if self.0.join("Cargo.toml").exists() {
            let _ = Command::new("cargo")
                .args(["clean", "--offline", "--target-dir"])
                .arg(self.0.join("target"))
                .current_dir(&self.0)
                .status();
        }
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

fn packages(path: &Path) -> Result<BTreeMap<(String, String, String), Option<String>>> {
    let lock: toml::Value = toml::from_str(&fs::read_to_string(path)?)?;
    let entries = lock["package"].as_array().ok_or("invalid Cargo.lock")?;
    let mut result = BTreeMap::new();
    for package in entries {
        if let Some(source) = package.get("source").and_then(toml::Value::as_str) {
            result.insert(
                (
                    package["name"]
                        .as_str()
                        .ok_or("missing package name")?
                        .into(),
                    package["version"].as_str().ok_or("missing version")?.into(),
                    source.into(),
                ),
                package
                    .get("checksum")
                    .and_then(toml::Value::as_str)
                    .map(str::to_owned),
            );
        }
    }
    Ok(result)
}

fn cargo(root: &Path, arguments: &[&str]) -> Result<()> {
    if !Command::new("cargo")
        .args(arguments)
        .current_dir(root)
        .status()?
        .success()
    {
        return Err("consumer cargo command failed".into());
    }
    Ok(())
}

fn run() -> Result<()> {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if arguments.len() == 1 && arguments[0] == "--help" {
        println!(
            "cargo run --locked -p xtask -- foundation-consumer [--pdfium-library /absolute/path]"
        );
        return Ok(());
    }
    if arguments.first().is_none_or(|a| a != "foundation-consumer") {
        return Err("expected foundation-consumer [--pdfium-library /absolute/path]".into());
    }
    let library = match arguments.as_slice() {
        [_] => None,
        [_, flag, path] if flag == "--pdfium-library" => {
            let path = PathBuf::from(path);
            if !path.is_absolute() {
                return Err("--pdfium-library must be an explicit absolute file path".into());
            }
            Some(path)
        }
        _ => return Err("invalid foundation-consumer arguments".into()),
    };
    let feature = if library.is_some() { "pdfium" } else { "hayro" };
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or("missing repository")?;
    let directory = std::env::temp_dir().join(format!(
        "voltfill-consumer-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    ));
    fs::create_dir(&directory)?;
    let scratch = Scratch(directory);
    let root = &scratch.0;
    copy_tree(&repo.join("tests/foundation-consumer"), root)?;
    for name in ["Cargo.lock", "rust-toolchain.toml"] {
        fs::copy(repo.join(name), root.join(name))?;
    }
    let manifest = root.join("Cargo.toml");
    let crates = repo.join("crates");
    let crates = crates.to_str().ok_or("repository path is not UTF-8")?;
    fs::write(
        &manifest,
        fs::read_to_string(&manifest)?.replace("../../crates/", &format!("{crates}/")),
    )?;
    cargo(root, &["generate-lockfile", "--offline"])?;
    let locked = packages(&repo.join("Cargo.lock"))?;
    for (key, checksum) in packages(&root.join("Cargo.lock"))? {
        if locked.get(&key) != Some(&checksum) {
            return Err("consumer changed locked dependency versions or checksums".into());
        }
    }
    let metadata = Command::new("cargo")
        .args([
            "metadata",
            "--offline",
            "--locked",
            "--format-version",
            "1",
            "--features",
            feature,
        ])
        .current_dir(root)
        .stderr(Stdio::inherit())
        .output()?;
    if !metadata.status.success() {
        return Err("consumer metadata failed".into());
    }
    let metadata: serde_json::Value = serde_json::from_slice(&metadata.stdout)?;
    if metadata["workspace_members"]
        .as_array()
        .is_none_or(|members| members.len() != 1)
        || metadata["packages"]
            .as_array()
            .ok_or("missing packages")?
            .iter()
            .any(|p| p["name"] == "voltfill-core")
    {
        return Err("consumer is not independent of the extraction workspace".into());
    }
    let mut command = Command::new("cargo");
    command
        .args([
            "run",
            "--offline",
            "--locked",
            "--features",
            feature,
            "--target-dir",
        ])
        .arg(root.join("target"))
        .current_dir(root);
    if let Some(library) = library {
        command.arg("--").arg(library);
    }
    if !command.status()?.success() {
        return Err("consumer verification failed".into());
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
