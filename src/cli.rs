use crate::{
    paths,
    services::{Result, Services, error},
};
use std::{
    io::Read,
    path::{Path, PathBuf},
};

fn read(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let file = std::fs::File::open(path).map_err(error)?;
    if file.metadata().map_err(error)?.len() > limit {
        return Err("File exceeds size limit".into());
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(error)?;
    if bytes.len() as u64 > limit {
        return Err("File exceeds size limit".into());
    }
    Ok(bytes)
}
/// Restore an archive, or a legacy metadata JSON backup, without a review step.
/// Archives merge by default and legacy backups replace, as they always did.
fn restore(services: &Services, path: &Path, flags: &[String]) -> Result<()> {
    let (mut replace, mut use_backup, mut files) = (false, false, Vec::new());
    for flag in flags {
        match flag.as_str() {
            "--replace" => replace = true,
            "--use-backup" => use_backup = true,
            "--with-downloads" => files.push("downloads"),
            "--with-local-imports" => files.push("local_imports"),
            _ => return Err("Unknown option. Run nova-reader --help.".into()),
        }
    }
    let opened = crate::backup::plan::open(path)?;
    let mut choices = crate::backup::plan::default_choices(&opened)?;
    for choice in &mut choices {
        // Large file sets restore only when asked for, as they export.
        if choice.source == crate::backup::archive::DEVICE {
            choice
                .components
                .extend(files.iter().map(|name| (*name).to_owned()));
            choice.components.retain(|name| {
                crate::backup::plan::present(&opened, &choice.source).contains(name)
            });
        }
        choice.replace |= replace;
        if use_backup {
            choice.use_backup = choice.components.clone();
        }
    }
    let preview = crate::backup::plan::preview(services, std::sync::Arc::new(opened), choices)?;
    for line in preview.lines() {
        println!("{line}");
    }
    let applied = crate::backup::apply::apply(services, &preview)?;
    for warning in &applied.warnings {
        eprintln!("{warning}");
    }
    println!("Restored into {} profile(s).", applied.targets.len());
    Ok(())
}
pub fn run() -> Result<bool> {
    let mut args = std::env::args_os().skip(1);
    let Some(command) = args.next() else {
        return Ok(false);
    };
    let command = command.to_str().ok_or("Invalid command")?;
    if command == "--help" || command == "-h" {
        println!(
            "Nova Reader {}\n\nRun without arguments to open the reader.\n\n  install-addon FILE.zip\n  list-sources\n  import FILE.cbz\n  import-folder DIRECTORY\n  backup FILE.nova-reader-backup [--with-downloads] [--with-local-imports]\n  verify-backup FILE.nova-reader-backup\n  restore FILE [--replace] [--use-backup] [--with-downloads] [--with-local-imports]\n  undo-restore\n  catalogue-keiyoushi [HTTPS_INDEX_URL]\n\nSet NOVA_READER_DATA_DIR to use an independent application directory.\nSources are executable JavaScript packages; APK/JAR catalogues are discovery only.",
            env!("CARGO_PKG_VERSION")
        );
        return Ok(true);
    }
    if command == "--version" {
        println!("Nova Reader {}", env!("CARGO_PKG_VERSION"));
        return Ok(true);
    }
    if command == "catalogue-keiyoushi" {
        let url = args
            .next()
            .map(|v| v.to_string_lossy().into_owned())
            .unwrap_or_else(|| reader_catalogue::KEIYOUSHI_INDEX.into());
        let catalogue = reader_catalogue::CatalogueClient::new()
            .map_err(error)?
            .fetch_keiyoushi(&url)
            .map_err(error)?;
        println!(
            "{}",
            serde_json::to_string_pretty(&catalogue).map_err(error)?
        );
        return Ok(true);
    }
    let services = Services::open(&paths::data_dir()?)?;
    if command == "list-sources" {
        println!(
            "{}",
            serde_json::to_string_pretty(&services.sources.list().map_err(error)?)
                .map_err(error)?
        );
        return Ok(true);
    }
    if command == "undo-restore" {
        let preview = crate::backup::apply::undo_preview(&services)?;
        for line in preview.lines() {
            println!("{line}");
        }
        let applied = crate::backup::apply::apply(&services, &preview)?;
        println!("Undone for {} profile(s).", applied.targets.len());
        return Ok(true);
    }
    let path = PathBuf::from(
        args.next()
            .ok_or("This command requires a file or directory")?,
    );
    let flags: Vec<String> = args
        .map(|flag| flag.to_string_lossy().into_owned())
        .collect();
    if !flags.is_empty() && !matches!(command, "restore" | "backup") {
        return Err("Unexpected extra arguments".into());
    }
    match command {
        "install-addon" => {
            let id = services.install(&read(&path, 8 * 1024 * 1024)?)?;
            println!("Installed {id}");
        }
        "import" | "import-folder" => {
            let id = services.import_local(&path, command == "import-folder")?;
            println!("Imported {}", id.local_id);
        }
        "backup" => {
            let profiles = crate::profiles::list()?;
            let mut files = crate::backup::Files::default();
            for flag in &flags {
                match flag.as_str() {
                    "--with-downloads" => files.downloads = true,
                    "--with-local-imports" => files.local = true,
                    _ => return Err("Unknown option. Run nova-reader --help.".into()),
                }
            }
            let selection = crate::backup::Selection::everything(&profiles, files);
            let exported = crate::backup::export(&services, &selection, &path)?;
            println!(
                "Saved {} profile(s), {} bytes. The archive includes addon credentials in plaintext.",
                exported.profiles, exported.bytes
            );
        }
        "verify-backup" => println!("{}", crate::backup::describe(&path)?),
        "restore" => restore(&services, &path, &flags)?,
        _ => return Err("Unknown command. Run nova-reader --help.".into()),
    }
    Ok(true)
}
