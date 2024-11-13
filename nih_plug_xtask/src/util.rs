use anyhow::{Context, Result};
use std::fs;
use std::path::Path;
use std::process::Command;

use crate::{Architecture, CompilationTarget, PluginType};

/// Acts the same as [`reflink::reflink_or_copy()`], but it removes existing files first. This works
/// around a limitation of macOS that the reflink crate also applies to other platforms to stay
/// consistent. See the [`reflink`] crate documentation or #26 for more information.
pub fn reflink<P: AsRef<Path>, Q: AsRef<Path>>(from: P, to: Q) -> Result<Option<u64>> {
    let to = to.as_ref();
    if to.exists() {
        fs::remove_file(to).context("Could not remove file before reflinking")?;
    }

    reflink::reflink_or_copy(from, to).context("Could not reflink or copy file")
}

/// Either reflink `from` to `to` if `from` contains a single element, or combine multiple binaries
/// into `to` depending on the compilation target
pub fn reflink_or_combine<P: AsRef<Path>>(
    from: &[&Path],
    to: P,
    compilation_target: CompilationTarget,
) -> Result<()> {
    match (from, compilation_target) {
        ([], _) => anyhow::bail!("The 'from' slice is empty"),
        ([path], _) => {
            reflink(path, to.as_ref()).with_context(|| {
                format!(
                    "Could not copy {} to {}",
                    path.display(),
                    to.as_ref().display()
                )
            })?;
        }
        (paths, CompilationTarget::MacOSUniversal) => {
            lipo(paths, to.as_ref())
                .with_context(|| format!("Could not create universal binary from {paths:?}"))?;
        }
        _ => anyhow::bail!(
            "Combining multiple binaries is not yet supported for {compilation_target:?}."
        ),
    };

    Ok(())
}

/// Combine multiple macOS binaries into a universal macOS binary.
pub fn lipo(inputs: &[&Path], target: &Path) -> Result<()> {
    let status = Command::new("lipo")
        .arg("-create")
        .arg("-output")
        .arg(target)
        .args(inputs)
        .status()
        .context("Could not call the 'lipo' binary to create a universal macOS binary")?;
    if !status.success() {
        anyhow::bail!(
            "Could not call the 'lipo' binary to create a universal macOS binary from {inputs:?}",
        );
    } else {
        Ok(())
    }
}

trait PluginInstallPaths {
    const AU_64: Option<&'static str> = None;
    const AU_32: Option<&'static str> = None;

    const CLAP_64: &'static str;
    const CLAP_32: &'static str;

    const VST2_64: &'static str;
    const VST2_32: &'static str;

    const VST3_64: &'static str;
    const VST3_32: &'static str;
}

struct PluginInstallPathsLinux;

impl PluginInstallPaths for PluginInstallPathsLinux {
    const CLAP_64: &'static str = concat!(env!("HOME"), "/.clap");
    const CLAP_32: &'static str = Self::CLAP_64;

    const VST2_64: &'static str = concat!(env!("HOME"), "/.vst");
    const VST2_32: &'static str = Self::VST2_64;

    const VST3_64: &'static str = concat!(env!("HOME"), "/.vst3");
    const VST3_32: &'static str = Self::VST3_64;
}

struct PluginInstallPathsMacOS;

impl PluginInstallPaths for PluginInstallPathsMacOS {
    const AU_64: Option<&'static str> =
        Some(concat!(env!("HOME"), "/Library/Audio/Plug-Ins/Components"));
    const AU_32: Option<&'static str> = Self::AU_64;

    const CLAP_64: &'static str = concat!(env!("HOME"), "/Library/Audio/Plug-Ins/CLAP");
    const CLAP_32: &'static str = Self::CLAP_64;

    const VST2_64: &'static str = concat!(env!("HOME"), "/Library/Audio/Plug-Ins/VST");
    const VST2_32: &'static str = Self::VST2_64;

    const VST3_64: &'static str = concat!(env!("HOME"), "/Library/Audio/Plug-Ins/VST3");
    const VST3_32: &'static str = Self::VST3_64;
}

struct PluginInstallPathsWindows;

impl PluginInstallPaths for PluginInstallPathsWindows {
    const CLAP_64: &'static str = "C:/Program Files/Common Files/CLAP";
    const CLAP_32: &'static str = "C:/Program Files (x86)/Common Files/CLAP";

    const VST2_64: &'static str = "C:/Program Files/Common Files/VST2";
    const VST2_32: &'static str = "C:/Program Files (x86)/Common Files/VST2";

    const VST3_64: &'static str = "C:/Program Files/Common Files/VST3";
    const VST3_32: &'static str = "C:/Program Files (x86)/Common Files/VST3";
}

macro_rules! plugin_install_path {
    ($target:expr, $path_32:ident, $path_64:ident) => {
        match $target {
            CompilationTarget::Linux(architecture) => match architecture {
                Architecture::X86 => PluginInstallPathsLinux::$path_32,
                _ => PluginInstallPathsLinux::$path_64,
            },
            CompilationTarget::MacOS(architecture) => match architecture {
                Architecture::X86 => PluginInstallPathsMacOS::$path_32,
                _ => PluginInstallPathsMacOS::$path_64,
            },
            CompilationTarget::MacOSUniversal => PluginInstallPathsMacOS::$path_64,
            CompilationTarget::Windows(architecture) => match architecture {
                Architecture::X86 => PluginInstallPathsWindows::$path_32,
                _ => PluginInstallPathsWindows::$path_64,
            },
        }
    };
}

fn plugin_install_path_au(compilation_target: CompilationTarget) -> Option<&'static str> {
    plugin_install_path!(compilation_target, AU_32, AU_64)
}

fn plugin_install_path_clap(compilation_target: CompilationTarget) -> &'static str {
    plugin_install_path!(compilation_target, CLAP_32, CLAP_64)
}

fn plugin_install_path_vst2(compilation_target: CompilationTarget) -> &'static str {
    plugin_install_path!(compilation_target, VST2_32, VST2_64)
}

fn plugin_install_path_vst3(compilation_target: CompilationTarget) -> &'static str {
    plugin_install_path!(compilation_target, VST3_32, VST3_64)
}

pub fn install_plugin(
    bundle_name: &String,
    bundle_home: &Path,
    plugin_type: PluginType,
    compilation_target: CompilationTarget,
) {
    let install_root: &str;
    let plugin_type_name: &str;
    let extension: &str;

    match plugin_type {
        PluginType::Au => {
            let install_path_au = plugin_install_path_au(compilation_target);
            if install_path_au.is_none() {
                return;
            }
            install_root = install_path_au.unwrap();
            plugin_type_name = "AU";
            extension = ".component";
        }
        PluginType::Clap => {
            install_root = plugin_install_path_clap(compilation_target);
            plugin_type_name = "CLAP";
            extension = ".clap";
        }
        PluginType::Vst2 => {
            install_root = plugin_install_path_vst2(compilation_target);
            plugin_type_name = "VST2";
            extension = ".vst";
        }
        PluginType::Vst3 => {
            install_root = plugin_install_path_vst3(compilation_target);
            plugin_type_name = "VST3";
            extension = ".vst3";
        }
    };

    let install_path = Path::new("")
        .join(install_root)
        .join(bundle_name.clone() + extension);
    let result = dircpy::copy_dir_advanced(
        &bundle_home,
        &install_path,
        true,
        false,
        false,
        vec![],
        vec![],
    );

    match result {
        Ok(_) => eprintln!(
            "Copied the {plugin_type_name} bundle to '{}'",
            install_path.display()
        ),
        Err(error) => eprintln!(
            "Could not copy {plugin_type_name} bundle to '{}'. Error: '{}'.",
            install_path.display(),
            error.to_string()
        ),
    }

    // NOTE: For readability.
    println!("");
}
