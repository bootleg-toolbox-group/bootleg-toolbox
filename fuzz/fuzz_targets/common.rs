// Shared by the fuzz targets: the readers take paths, so each input is written to a file
// in a temporary folder of this process.

use std::path::PathBuf;

pub fn write(name: &str, data: &[u8]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bootleg-toolbox-fuzz-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, data).unwrap();
    path
}
