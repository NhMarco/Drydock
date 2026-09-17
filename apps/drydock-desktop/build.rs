fn main() {
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
