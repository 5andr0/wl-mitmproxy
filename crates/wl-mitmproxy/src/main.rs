use std::{
    env, fs, io,
    os::{
        fd::OwnedFd,
        unix::{
            fs::FileTypeExt,
            net::{UnixListener, UnixStream},
            process::CommandExt,
            process::ExitStatusExt,
        },
    },
    path::{Path, PathBuf},
    process::{Command, ExitCode, Stdio},
    rc::Rc,
    sync::{
        atomic::{AtomicBool, AtomicI32, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};

use clap::Parser;
use signal_hook::{
    consts::signal::{SIGINT, SIGTERM},
    iterator::Signals,
};
use wl_proxy::{
    baseline::Baseline,
    client::ClientHandler,
    object::{ConcreteObject, Object, ObjectCoreApi, ObjectRcUtils},
    protocols::{
        wayland::{
            wl_display::{WlDisplay, WlDisplayHandler},
            wl_registry::{WlRegistry, WlRegistryHandler},
            wl_surface::WlSurface,
        },
        xdg_shell::{
            xdg_surface::{XdgSurface, XdgSurfaceHandler},
            xdg_toplevel::{XdgToplevel, XdgToplevelHandler},
            xdg_wm_base::{XdgWmBase, XdgWmBaseHandler},
        },
    },
    state::State,
};

#[derive(Debug, Parser)]
#[command(name = "wl-mitmproxy")]
#[command(about = "Wayland proxy that rewrites selected protocol messages")]
#[command(override_usage = "wl-mitmproxy [OPTIONS]\n       wl-mitmproxy [OPTIONS] -- <COMMAND> [ARGS...]")]
#[command(after_help = "At least one rewrite option must be supplied: --app-id and/or --title.")]
struct Cli {
        #[arg(long, value_name = "APP_ID")]
        app_id: Option<String>,

        #[arg(long, value_name = "TITLE")]
        title: Option<String>,

        #[arg(long, env = "WAYLAND_DISPLAY_PROXY", value_name = "NAME_OR_PATH")]
        proxy_socket: Option<String>,

        #[arg(long, env = "XDG_RUNTIME_DIR_PROXY", value_name = "PATH")]
        proxy_runtime_dir: Option<PathBuf>,

        #[arg(long, hide = true)]
        foreground_daemon: bool,

        #[arg(last = true, allow_hyphen_values = true, value_name = "COMMAND")]
        command: Vec<std::ffi::OsString>,
    }

    #[derive(Clone)]
    struct HostEnvironment {
        runtime_dir: PathBuf,
        display: String,
    }

    #[derive(Clone)]
    struct RewriteConfig {
        app_id: Option<String>,
        title: Option<String>,
    }

    struct SocketPathGuard {
        path: PathBuf,
    }

    impl Drop for SocketPathGuard {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
        }
    }

    struct SessionClientHandler {
        state: Rc<State>,
    }

    impl ClientHandler for SessionClientHandler {
        fn disconnected(self: Box<Self>) {
            self.state.destroy();
        }
    }

    struct DisplayProxy {
        rewrite: RewriteConfig,
    }

    impl WlDisplayHandler for DisplayProxy {
        fn handle_get_registry(&mut self, slf: &Rc<WlDisplay>, registry: &Rc<WlRegistry>) {
            registry.set_handler(RegistryProxy {
                rewrite: self.rewrite.clone(),
            });
            slf.send_get_registry(registry);
        }
    }

    struct RegistryProxy {
        rewrite: RewriteConfig,
    }

impl WlRegistryHandler for RegistryProxy {
    fn handle_bind(&mut self, slf: &Rc<WlRegistry>, name: u32, id: Rc<dyn Object>) {
        if id.interface() == XdgWmBase::INTERFACE {
            id.downcast::<XdgWmBase>().set_handler(XdgWmBaseProxy {
                rewrite: self.rewrite.clone(),
            });
        }
        slf.send_bind(name, id);
    }
}

    struct XdgWmBaseProxy {
        rewrite: RewriteConfig,
    }

impl XdgWmBaseHandler for XdgWmBaseProxy {
    fn handle_get_xdg_surface(
        &mut self,
        slf: &Rc<XdgWmBase>,
        id: &Rc<XdgSurface>,
        surface: &Rc<WlSurface>,
    ) {
        id.set_handler(XdgSurfaceProxy {
            rewrite: self.rewrite.clone(),
        });
        slf.send_get_xdg_surface(id, surface);
    }
}

    struct XdgSurfaceProxy {
        rewrite: RewriteConfig,
    }

impl XdgSurfaceHandler for XdgSurfaceProxy {
    fn handle_get_toplevel(&mut self, slf: &Rc<XdgSurface>, id: &Rc<XdgToplevel>) {
        id.set_handler(XdgToplevelProxy {
            rewrite: self.rewrite.clone(),
        });
        slf.send_get_toplevel(id);
    }
}

    struct XdgToplevelProxy {
        rewrite: RewriteConfig,
    }

impl XdgToplevelHandler for XdgToplevelProxy {
    fn handle_set_app_id(&mut self, slf: &Rc<XdgToplevel>, app_id: &str) {
        let forwarded_app_id = self.rewrite.app_id.as_deref().unwrap_or(app_id);
        if let Err(err) = slf.try_send_set_app_id(forwarded_app_id) {
            eprintln!(
                "wl-mitmproxy: failed to forward xdg_toplevel.set_app_id {:?}: {}",
                forwarded_app_id,
                err,
            );
        }
    }

    fn handle_set_title(&mut self, slf: &Rc<XdgToplevel>, title: &str) {
        let forwarded_title = self.rewrite.title.as_deref().unwrap_or(title);
        if let Err(err) = slf.try_send_set_title(forwarded_title) {
            eprintln!(
                "wl-mitmproxy: failed to forward xdg_toplevel.set_title {:?}: {}",
                forwarded_title,
                err,
            );
        }
    }
}

    struct ResolvedSocket {
        display_value: String,
        listen_path: PathBuf,
    }

    enum SocketState {
        Available,
        Active,
        Stale,
    }

    #[derive(Clone)]
    struct ShutdownSignal {
        requested: Arc<AtomicBool>,
        last_signal: Arc<AtomicI32>,
    }

    fn main() -> ExitCode {
        match run() {
            Ok(code) => code,
            Err(err) => {
                eprintln!("wl-mitmproxy: {err}");
                ExitCode::FAILURE
            }
        }
    }

    fn run() -> Result<ExitCode, Box<dyn std::error::Error>> {
        let cli = Cli::parse();
        let host = read_host_environment()?;
        let rewrite = RewriteConfig::from_cli(&cli)?;
        let shutdown_signal = install_shutdown_signal()?;
        let proxy_runtime_dir = cli
            .proxy_runtime_dir
            .clone()
            .unwrap_or_else(|| host.runtime_dir.clone());
        let proxy_runtime_dir_overridden = cli.proxy_runtime_dir.is_some();

        if cli.command.is_empty() && !cli.foreground_daemon {
            return spawn_daemon_process(&cli, &host, &proxy_runtime_dir);
        }

        if cli.command.is_empty() {
            run_daemon_mode(&cli, &host, &proxy_runtime_dir, rewrite, shutdown_signal)?;
            return Ok(ExitCode::SUCCESS);
        }

        run_child_mode(
            &cli,
            &host,
            &proxy_runtime_dir,
            proxy_runtime_dir_overridden,
            rewrite,
            shutdown_signal,
        )
    }

    impl RewriteConfig {
        fn from_cli(cli: &Cli) -> Result<Self, io::Error> {
            if cli.app_id.is_none() && cli.title.is_none() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "at least one rewrite option must be supplied: --app-id and/or --title",
                ));
            }

            Ok(Self {
                app_id: cli.app_id.clone(),
                title: cli.title.clone(),
            })
        }
    }

    fn spawn_daemon_process(
        cli: &Cli,
        host: &HostEnvironment,
        proxy_runtime_dir: &Path,
    ) -> Result<ExitCode, Box<dyn std::error::Error>> {
        let socket = match cli.proxy_socket.as_deref() {
            Some(spec) => resolve_socket_spec(proxy_runtime_dir, spec),
            None => resolve_socket_spec(proxy_runtime_dir, &default_proxy_socket_name(&host.display)),
        };

        let mut command = Command::new(env::current_exe()?);
        if let Some(app_id) = cli.app_id.as_deref() {
            command.arg("--app-id").arg(app_id);
        }
        if let Some(title) = cli.title.as_deref() {
            command.arg("--title").arg(title);
        }
        if let Some(proxy_socket) = cli.proxy_socket.as_deref() {
            command.arg("--proxy-socket").arg(proxy_socket);
        }
        if let Some(proxy_runtime_dir) = cli.proxy_runtime_dir.as_deref() {
            command.arg("--proxy-runtime-dir").arg(proxy_runtime_dir);
        }
        command.arg("--foreground-daemon");

        let devnull = fs::File::options().read(true).write(true).open("/dev/null")?;
        command.stdin(Stdio::from(devnull.try_clone()?));
        command.stdout(Stdio::from(devnull.try_clone()?));
        command.stderr(Stdio::from(devnull));

        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }

        command.spawn()?;

        eprintln!(
            "wl-mitmproxy daemon started on {} ({})",
            socket.display_value,
            socket.listen_path.display()
        );

        Ok(ExitCode::SUCCESS)
    }

    fn run_daemon_mode(
        cli: &Cli,
        host: &HostEnvironment,
        proxy_runtime_dir: &Path,
        rewrite: RewriteConfig,
        shutdown_signal: ShutdownSignal,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let socket = match cli.proxy_socket.as_deref() {
            Some(spec) => resolve_socket_spec(proxy_runtime_dir, spec),
            None => resolve_socket_spec(proxy_runtime_dir, &default_proxy_socket_name(&host.display)),
        };

        let (listener, _guard) = prepare_listener(&socket.listen_path)?;
        eprintln!(
            "wl-mitmproxy listening on {} ({})",
            socket.display_value,
            socket.listen_path.display()
        );

        serve(
            listener,
            &host.display,
            rewrite,
            Some(shutdown_signal.requested),
        )?;
        Ok(())
    }

    fn run_child_mode(
        cli: &Cli,
        host: &HostEnvironment,
        proxy_runtime_dir: &Path,
        proxy_runtime_dir_overridden: bool,
        rewrite: RewriteConfig,
        shutdown_signal: ShutdownSignal,
    ) -> Result<ExitCode, Box<dyn std::error::Error>> {
        let socket = match cli.proxy_socket.as_deref() {
            Some(spec) => resolve_socket_spec(proxy_runtime_dir, spec),
            None => allocate_incrementing_socket(proxy_runtime_dir, &default_proxy_socket_name(&host.display))?,
        };

        let (listener, guard) = prepare_listener(&socket.listen_path)?;
        let shutdown = Arc::new(AtomicBool::new(false));
        let server_shutdown = shutdown.clone();
        let target_display = host.display.clone();
        let rewrite_config = rewrite.clone();

        let server_thread = thread::spawn(move || serve(listener, &target_display, rewrite_config, Some(server_shutdown)));

        eprintln!(
            "wl-mitmproxy launching child with WAYLAND_DISPLAY={} ({})",
            socket.display_value,
            socket.listen_path.display()
        );

        let mut command = Command::new(&cli.command[0]);
        command.args(&cli.command[1..]);
        command.env("WAYLAND_DISPLAY", &socket.display_value);
        if proxy_runtime_dir_overridden {
            command.env("XDG_RUNTIME_DIR", proxy_runtime_dir);
        }

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(err) => {
                shutdown.store(true, Ordering::Relaxed);
                join_server_thread(server_thread)?;
                drop(guard);
                return Err(Box::new(err));
            }
        };

        let status = wait_for_child_or_signal(&mut child, &shutdown_signal)?;
        shutdown.store(true, Ordering::Relaxed);
        join_server_thread(server_thread)?;
        drop(guard);

        Ok(exit_code_from_status(status))
    }

    fn serve(
        listener: UnixListener,
        target_display: &str,
        rewrite: RewriteConfig,
        shutdown: Option<Arc<AtomicBool>>,
    ) -> io::Result<()> {
        if shutdown.is_some() {
            listener.set_nonblocking(true)?;
        }

        loop {
            if let Some(shutdown) = shutdown.as_ref() {
                if shutdown.load(Ordering::Relaxed) {
                    return Ok(());
                }
            }

            match listener.accept() {
                Ok((stream, _)) => {
                    let target_display = target_display.to_owned();
                    let rewrite = rewrite.clone();
                    thread::spawn(move || {
                        if let Err(err) = run_client_session(stream, &target_display, &rewrite) {
                            eprintln!("wl-mitmproxy session failed: {err}");
                        }
                    });
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(50));
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => return Err(err),
            }
        }
    }

fn run_client_session(
    stream: UnixStream,
    target_display: &str,
    rewrite: &RewriteConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    stream.set_nonblocking(true)?;
    let client_socket: Rc<OwnedFd> = Rc::new(stream.into());
    let state = State::builder(Baseline::ALL_OF_THEM)
        .with_server_display_name(target_display)
        .with_log_prefix("wl-mitmproxy")
        .build()?;
    let _destructor = state.create_destructor();
    let client = state.add_client(&client_socket)?;
    client.set_handler(SessionClientHandler {
        state: state.clone(),
    });
    client.display().set_handler(DisplayProxy {
        rewrite: rewrite.clone(),
    });

    loop {
        match state.dispatch_blocking() {
            Ok(_) => {}
            Err(err) if err.is_destroyed() => return Ok(()),
            Err(err) => return Err(Box::new(err)),
        }
    }
}

    fn read_host_environment() -> Result<HostEnvironment, Box<dyn std::error::Error>> {
        let runtime_dir = PathBuf::from(required_env("XDG_RUNTIME_DIR")?);
        let display = required_env("WAYLAND_DISPLAY")?;
        Ok(HostEnvironment {
            runtime_dir,
            display,
        })
    }

    fn required_env(name: &str) -> Result<String, io::Error> {
        env::var(name).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("required environment variable {name} is not set"),
            )
        })
    }

    fn install_shutdown_signal() -> Result<ShutdownSignal, Box<dyn std::error::Error>> {
        let mut signals = Signals::new([SIGINT, SIGTERM])?;
        let requested = Arc::new(AtomicBool::new(false));
        let last_signal = Arc::new(AtomicI32::new(0));
        let requested_clone = requested.clone();
        let last_signal_clone = last_signal.clone();

        thread::spawn(move || {
            for signal in signals.forever() {
                last_signal_clone.store(signal, Ordering::Relaxed);
                requested_clone.store(true, Ordering::Relaxed);
            }
        });

        Ok(ShutdownSignal {
            requested,
            last_signal,
        })
    }

    fn default_proxy_socket_name(host_display: &str) -> String {
        let name = Path::new(host_display)
            .file_name()
            .and_then(|value| value.to_str())
            .filter(|value| !value.is_empty())
            .unwrap_or("wayland-0");
        format!("{name}-proxy")
    }

    fn resolve_socket_spec(proxy_runtime_dir: &Path, spec: &str) -> ResolvedSocket {
        let path = PathBuf::from(spec);
        if path.is_absolute() {
            ResolvedSocket {
                display_value: spec.to_owned(),
                listen_path: path,
            }
        } else {
            ResolvedSocket {
                display_value: spec.to_owned(),
                listen_path: proxy_runtime_dir.join(spec),
            }
        }
    }

    fn allocate_incrementing_socket(
        proxy_runtime_dir: &Path,
        base_name: &str,
    ) -> Result<ResolvedSocket, io::Error> {
        for index in 0..u32::MAX {
            let candidate = if index == 0 {
                base_name.to_owned()
            } else {
                format!("{base_name}-{index}")
            };

            let resolved = resolve_socket_spec(proxy_runtime_dir, &candidate);
            match socket_state(&resolved.listen_path)? {
                SocketState::Available => return Ok(resolved),
                SocketState::Active => {}
                SocketState::Stale => {
                    eprintln!("reusing stale socket {}", resolved.listen_path.display());
                    fs::remove_file(&resolved.listen_path)?;
                    return Ok(resolved);
                }
            }
        }

        Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            format!("could not allocate a free proxy socket under {}", proxy_runtime_dir.display()),
        ))
    }

    fn prepare_listener(path: &Path) -> io::Result<(UnixListener, SocketPathGuard)> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }

        match socket_state(path)? {
            SocketState::Available => {}
            SocketState::Active => {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    format!("socket already exists and is active: {}", path.display()),
                ));
            }
            SocketState::Stale => {
                eprintln!("reusing stale socket {}", path.display());
                fs::remove_file(path)?;
            }
        }

        let listener = UnixListener::bind(path)?;
        Ok((
            listener,
            SocketPathGuard {
                path: path.to_path_buf(),
            },
        ))
    }

    fn socket_state(path: &Path) -> io::Result<SocketState> {
        let metadata = match fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(SocketState::Available),
            Err(err) => return Err(err),
        };

        if !metadata.file_type().is_socket() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("path exists and is not a socket: {}", path.display()),
            ));
        }

        match UnixStream::connect(path) {
            Ok(stream) => {
                drop(stream);
                Ok(SocketState::Active)
            }
            Err(err) if err.kind() == io::ErrorKind::ConnectionRefused => Ok(SocketState::Stale),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(SocketState::Available),
            Err(err) => Err(err),
        }
    }

    fn join_server_thread(handle: thread::JoinHandle<io::Result<()>>) -> Result<(), Box<dyn std::error::Error>> {
        match handle.join() {
            Ok(result) => result.map_err(|err| Box::new(err) as Box<dyn std::error::Error>),
            Err(_) => Err(Box::new(io::Error::new(
                io::ErrorKind::Other,
                "proxy server thread panicked",
            ))),
        }
    }

    fn wait_for_child_or_signal(
        child: &mut std::process::Child,
        shutdown_signal: &ShutdownSignal,
    ) -> io::Result<std::process::ExitStatus> {
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok(status);
            }

            if shutdown_signal.requested.load(Ordering::Relaxed) {
                let signal = match shutdown_signal.last_signal.load(Ordering::Relaxed) {
                    0 => SIGTERM,
                    value => value,
                };

                forward_signal(child.id(), signal)?;
                return child.wait();
            }

            thread::sleep(Duration::from_millis(50));
        }
    }

    fn forward_signal(pid: u32, signal: i32) -> io::Result<()> {
        let result = unsafe { libc::kill(pid as i32, signal) };
        if result == -1 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                return Ok(());
            }
            return Err(error);
        }
        Ok(())
    }

    fn exit_code_from_status(status: std::process::ExitStatus) -> ExitCode {
        if let Some(code) = status.code() {
            return ExitCode::from(code as u8);
        }

        if let Some(signal) = status.signal() {
            eprintln!("child terminated by signal {signal}");
        }

        ExitCode::FAILURE
    }