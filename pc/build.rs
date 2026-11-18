// SPDX-License-Identifier: GPL-3.0-or-later
// Windows: give xidpipe.exe a version resource (name, version, description, licence) like other
// Windows programs have, so its Properties > Details tab isn't empty. Only fixed public values go
// in; nothing about the machine or the person building it.
fn main() {
    #[cfg(windows)] // the build script runs on the build machine; rc.exe only exists on Windows
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let version = env!("CARGO_PKG_VERSION");
        winresource::WindowsResource::new()
            .set("ProductName", "xidpipe")
            .set("FileDescription", "xidpipe")
            .set("ProductVersion", version)
            .set("FileVersion", version)
            .set("CompanyName", "xidpipe")
            .set("InternalName", "xidpipe")
            .set("OriginalFilename", "xidpipe.exe")
            .set("LegalCopyright", "Licensed under GPL-3.0-or-later")
            .set("Comments", env!("CARGO_PKG_DESCRIPTION"))
            .compile()
            .expect("Windows version resource (needs rc.exe from the Windows SDK)");
    }
}
