// Release tooling, run by hand on the developer's machine:
//   cargo xtask keygen             once: makes the signing key, writes its public half to resources/release.toml
//   cargo xtask release [--dry-run] [--notes FILE]
//                                  builds the installer, the portable zip and the update files,
//                                  signs manifest.json, publishes GitHub release v<version> with `gh`
// The private key stays in %USERPROFILE%\.zapretUI\release.key and never enters the repository.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail, ensure};
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{Ed25519KeyPair, KeyPair};
use rkn_core::paths::{APP_EXE, PORTABLE_MARK, PRODUCT, SERVICE_EXE};
use rkn_core::release::{FileEntry, MANIFEST, Manifest, Place, SIGNATURE, hex, unhex};
use serde::Deserialize;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn release_toml() -> PathBuf {
    root().join("resources").join("release.toml")
}

fn key_file() -> anyhow::Result<PathBuf> {
    let home = std::env::var_os("USERPROFILE").context("нет USERPROFILE")?;
    Ok(PathBuf::from(home).join(".zapretUI").join("release.key"))
}

#[derive(Deserialize)]
struct Source {
    repo: String,
    key: String,
}

fn run(cmd: &mut Command) -> anyhow::Result<()> {
    let shown = format!("{cmd:?}");
    let status = cmd.status().with_context(|| format!("не запустился {shown}"))?;
    ensure!(status.success(), "{shown} завершился с ошибкой");
    Ok(())
}

fn sha256(path: &Path) -> anyhow::Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("чтение {}", path.display()))?;
    Ok(hex(ring::digest::digest(&ring::digest::SHA256, &bytes).as_ref()))
}

fn keygen() -> anyhow::Result<()> {
    let file = key_file()?;
    ensure!(!file.exists(), "ключ уже есть: {}. Новый ключ сделает старые сборки глухими к обновлениям", file.display());
    let mut seed = [0u8; 32];
    SystemRandom::new().fill(&mut seed).map_err(|_| anyhow::anyhow!("нет источника случайных чисел"))?;
    let pair = Ed25519KeyPair::from_seed_unchecked(&seed).map_err(|_| anyhow::anyhow!("ключ не создан"))?;
    std::fs::create_dir_all(file.parent().context("путь ключа")?)?;
    std::fs::write(&file, hex(&seed))?;

    let path = release_toml();
    let text = std::fs::read_to_string(&path)?;
    let public = hex(pair.public_key().as_ref());
    let text: String = text
        .lines()
        .map(|l| if l.trim_start().starts_with("key =") { format!("key = \"{public}\"") } else { l.to_string() })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&path, text + "\n")?;
    println!("Закрытый ключ: {}", file.display());
    println!("Сохраните его копию в надёжном месте: без него новые версии не подписать.");
    println!("Открытый ключ записан в {}", path.display());
    Ok(())
}

fn signing_key(public: &str) -> anyhow::Result<Ed25519KeyPair> {
    let file = key_file()?;
    let seed = std::fs::read_to_string(&file).with_context(|| format!("нет ключа {}: сначала cargo xtask keygen", file.display()))?;
    let seed = unhex(&seed).filter(|s| s.len() == 32).context("ключ повреждён")?;
    let pair = Ed25519KeyPair::from_seed_unchecked(&seed).map_err(|_| anyhow::anyhow!("ключ повреждён"))?;
    ensure!(hex(pair.public_key().as_ref()) == public.trim(), "ключ в {} не от этого закрытого ключа", release_toml().display());
    Ok(pair)
}

/// Engine files the service checks before every start, plus what has to travel with them.
fn engine_files(src: &Path) -> anyhow::Result<Vec<String>> {
    let sums = std::fs::read_to_string(src.join("SHA256SUMS")).context("resources/winws2/SHA256SUMS")?;
    let mut files: Vec<String> = sums
        .lines()
        .filter_map(|l| l.split_once(' ').map(|(_, n)| n.trim().trim_start_matches('*').to_string()))
        .collect();
    files.extend(["VERSION".to_string(), "LICENSE-zapret2.txt".to_string()]);
    for f in &files {
        ensure!(src.join(f).is_file(), "нет файла resources/winws2/{f}");
    }
    Ok(files)
}

fn copy(from: &Path, to: &Path) -> anyhow::Result<()> {
    if let Some(dir) = to.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::copy(from, to).with_context(|| format!("копирование {}", from.display()))?;
    Ok(())
}

fn copy_tree(from: &Path, to: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let path = entry?.path();
        let target = to.join(path.file_name().context("имя файла")?);
        if path.is_dir() {
            copy_tree(&path, &target)?;
        } else {
            copy(&path, &target)?;
        }
    }
    Ok(())
}

fn release(dry_run: bool, notes: Option<PathBuf>) -> anyhow::Result<()> {
    let source: Source = toml::from_str(&std::fs::read_to_string(release_toml())?)?;
    ensure!(dry_run || !source.repo.trim().is_empty(), "в resources/release.toml не указан repo");
    ensure!(!source.key.trim().is_empty(), "в resources/release.toml нет ключа: cargo xtask keygen");
    let pair = signing_key(&source.key)?;
    let tag = format!("v{VERSION}");

    // A clean build every time: Cargo may keep an exe from another version in target/release.
    run(Command::new("cargo").current_dir(root()).args(["clean", "--release", "-p", "rkn-svc", "-p", "rkn-app", "-p", "rkn-setup"]))?;
    run(Command::new("cargo").current_dir(root()).args(["build", "--release", "-p", "rkn-svc", "-p", "rkn-app"]))?;
    let built = root().join("target").join("release");
    let app = std::fs::read(built.join(APP_EXE))?;
    let stamp = format!("{PRODUCT} {VERSION}");
    ensure!(app.windows(stamp.len()).any(|w| w == stamp.as_bytes()), "{APP_EXE} собран не для версии {VERSION}");
    let winws2 = root().join("resources").join("winws2");
    let dist = root().join("target").join("dist");
    let _ = std::fs::remove_dir_all(&dist);
    std::fs::create_dir_all(&dist)?;

    // The program as it is laid out on disk: the payload of the installer and the portable folder.
    let program = dist.join("program");
    let mut entries: Vec<(FileEntry, PathBuf)> = Vec::new();
    for exe in [APP_EXE, SERVICE_EXE] {
        copy(&built.join(exe), &program.join(exe))?;
        let entry = FileEntry { place: Place::Bin, path: exe.to_string(), sha256: sha256(&built.join(exe))? };
        entries.push((entry, built.join(exe)));
    }
    for file in engine_files(&winws2)? {
        let from = file.split('/').fold(winws2.clone(), |p, part| p.join(part));
        copy(&from, &file.split('/').fold(program.join("winws2"), |p, part| p.join(part)))?;
        let entry = FileEntry { place: Place::Engine, path: file.clone(), sha256: sha256(&from)? };
        entries.push((entry, from));
    }

    // The installer: one exe with the program inside.
    run(Command::new("cargo").current_dir(root()).env("ZAPRETUI_PAYLOAD", &program).args(["build", "--release", "-p", "rkn-setup"]))?;
    let setup = dist.join(format!("{PRODUCT}-setup-{VERSION}.exe"));
    copy(&built.join(format!("{PRODUCT}-setup.exe")), &setup)?;

    // The portable copy: the same files, the marker, the license, packed into a zip.
    let name = format!("{PRODUCT}-portable-{VERSION}");
    let portable = dist.join(&name);
    copy_tree(&program, &portable)?;
    std::fs::write(portable.join(PORTABLE_MARK), "")?;
    if root().join("LICENSE").is_file() {
        copy(&root().join("LICENSE"), &portable.join("LICENSE"))?;
    }
    let zip = dist.join(format!("{name}.zip"));
    run(Command::new("tar").current_dir(&dist).arg("-a").arg("-c").arg("-f").arg(&zip).arg(&name))?;

    // Update files: flat assets, the manifest and its signature.
    let assets = dist.join("assets");
    let manifest = Manifest { version: VERSION.to_string(), files: entries.iter().map(|(e, _)| e.clone()).collect() };
    for (entry, from) in &entries {
        copy(from, &assets.join(entry.asset()))?;
    }
    let body = serde_json::to_vec_pretty(&manifest)?;
    std::fs::write(assets.join(MANIFEST), &body)?;
    std::fs::write(assets.join(SIGNATURE), pair.sign(&body).as_ref())?;

    let mut files: Vec<PathBuf> = vec![setup, zip];
    for entry in std::fs::read_dir(&assets)? {
        files.push(entry?.path());
    }
    println!("Готово: {}", dist.display());
    if dry_run {
        println!("--dry-run: релиз {tag} не опубликован");
        return Ok(());
    }
    let mut gh = Command::new("gh");
    gh.args(["release", "create", &tag, "--repo", source.repo.trim(), "--title", &format!("{PRODUCT} {VERSION}")]);
    match notes {
        Some(file) => gh.arg("--notes-file").arg(file),
        None => gh.args(["--notes", &format!("{PRODUCT} {VERSION}")]),
    };
    gh.args(&files);
    run(&mut gh)?;
    println!("Опубликован релиз {tag} в {}", source.repo.trim());
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| args.iter().any(|a| a == name);
    let notes = args.iter().position(|a| a == "--notes").and_then(|i| args.get(i + 1)).map(PathBuf::from);
    match args.first().map(String::as_str) {
        Some("keygen") => keygen(),
        Some("release") => release(flag("--dry-run"), notes),
        _ => bail!("cargo xtask keygen | release [--dry-run] [--notes FILE]"),
    }
}
