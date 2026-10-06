use std::path::PathBuf;

use hush_core::resolve_data_dir;

pub fn vault_dir_from_args(cli_dir: PathBuf) -> PathBuf {
    resolve_data_dir(&cli_dir)
}
