//! Windows: embeds the app icon (rendered from the same code as the tray icon), version info and a
//! manifest (modern controls and per-monitor DPI scaling for the settings window).

#[allow(dead_code)]
#[path = "src/ui.rs"]
mod ui;

#[allow(dead_code)]
#[path = "src/icons.rs"]
mod icons;

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=src/icons.rs");
    println!("cargo:rerun-if-changed=build.rs");
    let windows_target = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    // The resource compiler ships with Visual Studio; cross-checks from other hosts skip this.
    if !windows_target || !cfg!(windows) {
        return;
    }
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let ico = out.join("dictum.ico");
    std::fs::write(&ico, ico_file(&[16, 24, 32, 48, 64, 256])).unwrap();
    let manifest = out.join("dictum.manifest");
    std::fs::write(&manifest, MANIFEST).unwrap();
    let rc = out.join("dictum.rc");
    let version = env!("CARGO_PKG_VERSION");
    let commas = version.replace('.', ",");
    std::fs::write(
        &rc,
        format!(
            r#"1 ICON "{ico}"
1 24 "{manifest}"
1 VERSIONINFO
FILEVERSION {commas},0
PRODUCTVERSION {commas},0
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904b0"
    BEGIN
      VALUE "FileDescription", "Dictum - local dictation"
      VALUE "ProductName", "Dictum"
      VALUE "FileVersion", "{version}"
      VALUE "ProductVersion", "{version}"
      VALUE "OriginalFilename", "dictum.exe"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
            ico = ico.display().to_string().replace('\\', "\\\\"),
            manifest = manifest.display().to_string().replace('\\', "\\\\"),
        ),
    )
    .unwrap();
    embed_resource::compile(&rc, embed_resource::NONE).manifest_optional().unwrap();
}

const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0"
        processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*" />
    </dependentAssembly>
  </dependency>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
    </windowsSettings>
  </application>
</assembly>
"#;

/// An .ico with one 32-bit BMP image per size.
fn ico_file(sizes: &[u32]) -> Vec<u8> {
    let images: Vec<Vec<u8>> = sizes.iter().map(|&s| bmp_entry(s)).collect();
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0]);
    out.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (&size, image) in sizes.iter().zip(&images) {
        let dim = if size >= 256 { 0 } else { size as u8 };
        out.extend_from_slice(&[dim, dim, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes()); // planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&(image.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += image.len() as u32;
    }
    for image in images {
        out.extend_from_slice(&image);
    }
    out
}

fn bmp_entry(size: u32) -> Vec<u8> {
    let rgba = icons::rgba_sized(ui::Status::Ready, size);
    let mut out = Vec::new();
    // BITMAPINFOHEADER; height counts the colour bitmap plus the AND mask.
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(size as i32).to_le_bytes());
    out.extend_from_slice(&((size * 2) as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&[0; 24]);
    // BGRA rows, bottom-up.
    for y in (0..size).rev() {
        for x in 0..size {
            let i = ((y * size + x) * 4) as usize;
            out.extend_from_slice(&[rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]]);
        }
    }
    // AND mask (all zero: alpha channel decides), rows padded to 32 bits.
    let row = size.div_ceil(32) * 4;
    out.extend(std::iter::repeat_n(0u8, (row * size) as usize));
    out
}
