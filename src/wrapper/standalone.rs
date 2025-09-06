//! A standalone plugin target that directly connects to the system's audio and MIDI ports instead
//! of relying on a plugin host. This is mostly useful for quickly testing GUI changes.

use clap::{CommandFactory, FromArgMatches};

use self::backend::Backend;
use self::config::WrapperConfig;
use self::wrapper::{Wrapper, WrapperError};
use super::util::setup_logger;
use crate::prelude::Plugin;

mod backend;
mod config;
mod context;
mod wrapper;

/// Open an NIH-plug plugin as a standalone application. If the plugin has an editor, this will open
/// the editor and block until the editor is closed. Otherwise this will block until SIGINT is
/// received. This is mainly useful for quickly testing plugin GUIs. In order to use this, you will
/// first need to make your plugin's main struct `pub` and expose a `lib` artifact in addition to
/// your plugin's `cdylib`:
///
/// ```toml
/// # Cargo.toml
///
/// [lib]
/// # The `lib` artifact is needed for the standalone target
/// crate-type = ["cdylib", "lib"]
/// ```
///
/// You can then create a `src/main.rs` file that calls this function:
///
/// ```ignore
/// // src/main.rs
///
/// use nih_plug::prelude::*;
///
/// use plugin_name::PluginName;
///
/// fn main() {
///     nih_export_standalone::<PluginName>();
/// }
/// ```
///
/// By default this will connect to the 'default' audio and MIDI ports. Use the command line options
/// to change this. `--help` lists all available options.
///
/// If the wrapped plugin fails to initialize or throws an error during audio processing, then this
/// function will return `false`.
///
/// On macOS, a default app menu is created. Use [`nih_export_standalone_with_args_and_about`] to
/// set the metadata for the about box.
#[cfg(not(target_arch = "wasm32"))]
pub fn nih_export_standalone<P: Plugin>() -> bool {
    // TODO: If the backend fails to initialize then the standalones will exit normally instead of
    //       with an error code. This should probably be changed.
    nih_export_standalone_internal::<P, _>(std::env::args(), None)
}

/// The same as [`nih_export_standalone()`], but with the arguments taken from an iterator instead
/// of using [`std::env::args()`].
#[cfg(not(target_arch = "wasm32"))]
pub fn nih_export_standalone_with_args<P: Plugin, Args: IntoIterator<Item = String>>(
    args: Args,
) -> bool {
    nih_export_standalone_internal::<P, _>(args, None)
}

/// The same as [`nih_export_standalone_with_args()`], but with [`muda::AboutMetadata`] to set the
/// metadata for the about box of the default app menu.
#[cfg(all(target_os = "macos", not(target_arch = "wasm32")))]
pub fn nih_export_standalone_with_args_and_about<P: Plugin, Args: IntoIterator<Item = String>>(
    args: Args,
    about_metadata: muda::AboutMetadata,
) -> bool {
    nih_export_standalone_internal::<P, _>(args, Some(about_metadata))
}

#[cfg(not(target_arch = "wasm32"))]
fn nih_export_standalone_internal<P: Plugin, Args: IntoIterator<Item = String>>(
    args: Args,
    #[cfg(target_os = "macos")] about_metadata: Option<muda::AboutMetadata>,
) -> bool {
    setup_logger();

    #[cfg(target_os = "macos")]
    let _menu_bar = create_default_app_menu::<P>(about_metadata);

    // Instead of parsing this directly, we need to take a bit of a roundabout approach to get the
    // plugin's name and vendor in here since they'd otherwise be taken from NIH-plug's own
    // `Cargo.toml` file.
    let config = load_config_from_args::<P, Args>(args);

    match config.backend {
        config::BackendType::Auto => {
            let result = backend::Jack::new::<P>(config.clone()).map(|backend| {
                nih_log!("Using the JACK backend");
                run_wrapper::<P, _>(backend, config.clone())
            });

            #[cfg(target_os = "linux")]
            let result = result.or_else(|_| {
                match backend::CpalMidir::new::<P>(config.clone(), cpal::HostId::Alsa) {
                    Ok(backend) => {
                        nih_log!("Using the ALSA backend");
                        Ok(run_wrapper::<P, _>(backend, config.clone()))
                    }
                    Err(err) => {
                        nih_error!(
                            "Could not initialize either the JACK or the ALSA backends, falling \
                             back to the dummy audio backend: {err:#}"
                        );
                        Err(())
                    }
                }
            });
            #[cfg(target_os = "macos")]
            let result = result.or_else(|_| {
                match backend::CpalMidir::new::<P>(config.clone(), cpal::HostId::CoreAudio) {
                    Ok(backend) => {
                        nih_log!("Using the CoreAudio backend");
                        Ok(run_wrapper::<P, _>(backend, config.clone()))
                    }
                    Err(err) => {
                        nih_error!(
                            "Could not initialize either the JACK or the CoreAudio backends, \
                             falling back to the dummy audio backend: {err:#}"
                        );
                        Err(())
                    }
                }
            });
            #[cfg(target_os = "windows")]
            let result = result.or_else(|_| {
                match backend::CpalMidir::new::<P>(config.clone(), cpal::HostId::Wasapi) {
                    Ok(backend) => {
                        nih_log!("Using the WASAPI backend");
                        Ok(run_wrapper::<P, _>(backend, config.clone()))
                    }
                    Err(err) => {
                        nih_error!(
                            "Could not initialize either the JACK or the WASAPI backends, falling \
                             back to the dummy audio backend: {err:#}"
                        );
                        Err(())
                    }
                }
            });

            result.unwrap_or_else(|_| {
                nih_error!("Falling back to the dummy audio backend, audio and MIDI will not work");
                run_wrapper::<P, _>(backend::Dummy::new::<P>(config.clone()), config)
            })
        }
        config::BackendType::Jack => match backend::Jack::new::<P>(config.clone()) {
            Ok(backend) => run_wrapper::<P, _>(backend, config),
            Err(err) => {
                nih_error!("Could not initialize the JACK backend: {:#}", err);
                false
            }
        },
        #[cfg(target_os = "linux")]
        config::BackendType::Alsa => {
            match backend::CpalMidir::new::<P>(config.clone(), cpal::HostId::Alsa) {
                Ok(backend) => run_wrapper::<P, _>(backend, config),
                Err(err) => {
                    nih_error!("Could not initialize the ALSA backend: {:#}", err);
                    false
                }
            }
        }
        #[cfg(target_os = "macos")]
        config::BackendType::CoreAudio => {
            match backend::CpalMidir::new::<P>(config.clone(), cpal::HostId::CoreAudio) {
                Ok(backend) => run_wrapper::<P, _>(backend, config),
                Err(err) => {
                    nih_error!("Could not initialize the CoreAudio backend: {:#}", err);
                    false
                }
            }
        }
        #[cfg(target_os = "windows")]
        config::BackendType::Wasapi => {
            match backend::CpalMidir::new::<P>(config.clone(), cpal::HostId::Wasapi) {
                Ok(backend) => run_wrapper::<P, _>(backend, config),
                Err(err) => {
                    nih_error!("Could not initialize the WASAPI backend: {:#}", err);
                    false
                }
            }
        }
        config::BackendType::Dummy => {
            run_wrapper::<P, _>(backend::Dummy::new::<P>(config.clone()), config)
        }
    }
}

#[cfg(target_os = "macos")]
#[must_use]
fn create_default_app_menu<P: Plugin>(about_metadata: Option<muda::AboutMetadata>) -> muda::Menu {
    let menu_bar = muda::Menu::new();
    let app_menu = muda::Submenu::new("App", true);
    let result = menu_bar.append(&app_menu).and_then(|_| {
        app_menu.append_items(&[
            &muda::PredefinedMenuItem::about(
                Some(format!("&About {}", P::NAME).as_str()),
                about_metadata,
            ),
            &muda::PredefinedMenuItem::separator(),
            &muda::PredefinedMenuItem::services(None),
            &muda::PredefinedMenuItem::separator(),
            &muda::PredefinedMenuItem::hide(None),
            &muda::PredefinedMenuItem::hide_others(None),
            &muda::PredefinedMenuItem::show_all(None),
            &muda::PredefinedMenuItem::separator(),
            &muda::PredefinedMenuItem::quit(None),
        ])
    });
    if let Err(err) = result {
        nih_error!("Could not initialize the app menu: {err}");
    }
    menu_bar.init_for_nsapp();
    menu_bar
}

#[cfg(target_arch = "wasm32")]
pub fn nih_export_standalone<P: Plugin>() {
    nih_export_standalone_with_args::<P, Vec<_>>(None);
}

#[cfg(target_arch = "wasm32")]
pub fn nih_export_standalone_with_args<P: Plugin, Args: IntoIterator<Item = String> + 'static>(
    args: Option<Args>,
) {
    setup_logger();

    wasm_bindgen_futures::spawn_local(async {
        let config = load_config::<P, _>(args);

        match config.backend {
            config::BackendType::Auto => {
                match backend::CpalMidir::new::<P>(config.clone(), cpal::HostId::WebAudio).await {
                    Ok(backend) => {
                        nih_log!("Using the WebAudio backend");
                        run_wrapper::<P, _>(backend, config.clone()).await;
                    }
                    Err(err) => {
                        nih_error!(
                            "Could not initialize the WebAudio backends, falling back to the \
                             dummy audio backend: {err:#}"
                        );

                        nih_error!(
                            "Falling back to the dummy audio backend, audio and MIDI will not work"
                        );
                        run_wrapper::<P, _>(backend::Dummy::new::<P>(config.clone()), config).await;
                    }
                }
            }
            config::BackendType::WebAudio => {
                match backend::CpalMidir::new::<P>(config.clone(), cpal::HostId::WebAudio).await {
                    Ok(backend) => {
                        run_wrapper::<P, _>(backend, config).await;
                    }
                    Err(err) => {
                        nih_error!("Could not initialize the WebAudio backend: {:#}", err);
                    }
                }
            }
            config::BackendType::Dummy => {
                run_wrapper::<P, _>(backend::Dummy::new::<P>(config.clone()), config).await;
            }
        }
    });
}

#[cfg(target_arch = "wasm32")]
fn load_config<P: Plugin, Args: IntoIterator<Item = String>>(args: Option<Args>) -> WrapperConfig {
    if let Some(args) = args {
        load_config_from_args::<P, _>(args)
    } else {
        let config_string = js_sys::Reflect::get(&js_sys::global(), &"config".into()).unwrap();
        if let Some(config_string) = config_string.as_string() {
            if let Ok(config) = serde_json::from_str(&config_string) {
                return config;
            }
        }

        load_config_from_args::<P, _>(vec![])
    }
}

fn load_config_from_args<P: Plugin, Args: IntoIterator<Item = String>>(
    args: Args,
) -> WrapperConfig {
    WrapperConfig::from_arg_matches(
        &WrapperConfig::command()
            .name(P::NAME)
            .author(P::VENDOR)
            .get_matches_from(args),
    )
    .unwrap_or_else(|err| err.exit())
}

#[cfg(not(target_arch = "wasm32"))]
fn run_wrapper<P: Plugin, B: Backend<P>>(backend: B, config: WrapperConfig) -> bool {
    let wrapper = match Wrapper::<P, _>::new(backend, config) {
        Ok(wrapper) => wrapper,
        Err(err) => {
            print_error(err);
            return false;
        }
    };

    // TODO: Add a repl while the application is running to interact with parameters
    match wrapper.run() {
        Ok(()) => true,
        Err(err) => {
            print_error(err);
            false
        }
    }
}

#[cfg(target_arch = "wasm32")]
async fn run_wrapper<P: Plugin, B: Backend<P>>(backend: B, config: WrapperConfig) {
    let wrapper = match Wrapper::<P, _>::new(backend, config) {
        Ok(wrapper) => wrapper,
        Err(err) => {
            print_error(err);
            return;
        }
    };

    match wrapper.run().await {
        Ok(()) => (),
        Err(err) => print_error(err),
    }
}

fn print_error(error: WrapperError) {
    match error {
        WrapperError::InitializationFailed => {
            nih_error!("The plugin failed to initialize");
        }
    }
}
