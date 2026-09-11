fn main() {
    heal_git_index();
    #[cfg(windows)]
    {
        let mut resource = winres::WindowsResource::new();
        resource.set("ProductName", "Drydock");
        resource.set("FileDescription", "Drydock — Download, Activate, Play");
        resource.set("LegalCopyright", "Drydock contributors");
        resource.set_icon("../../assets/app-icon.ico");
        if let Err(error) = resource.compile() {
            println!("cargo:warning=Windows resources could not be embedded: {error}");
        }
    }
}

fn heal_git_index() {
    let mut dir = std::env::current_dir().ok();
    while let Some(current) = dir {
        let git_index = current.join(".git").join("index");
        if git_index.exists() {
            if let Ok(meta) = std::fs::metadata(&git_index) {
                if meta.len() == 0 {
                    let _ = std::fs::remove_file(&git_index);
                    let _ = std::process::Command::new("git")
                        .args(["reset"])
                        .current_dir(&current)
                        .status();
                    println!("cargo:warning=Auto-healed corrupted 0-byte .git/index file");
                }
            }
            break;
        }
        let git_lock = current.join(".git").join("index.lock");
        if git_lock.exists() {
            if let Ok(meta) = std::fs::metadata(&git_lock) {
                if meta.len() == 0 {
                    let _ = std::fs::remove_file(&git_lock);
                }
            }
        }
        dir = current.parent().map(|p| p.to_path_buf());
    }
}
