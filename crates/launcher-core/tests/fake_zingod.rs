//! fake-zingod, the stand-in for zingod in the zingod tests and the development `zeal-zingod`
//! that `scripts/dev-sidecar.sh` installs on Linux. check-versions.sh runs before anything
//! compiles, so this test is what checks the binary against the pin.

use std::process::{Command, Output};

const FAKE_ZINGOD: &str = env!("CARGO_BIN_EXE_fake-zingod");

fn run(args: &[&str]) -> Output {
    Command::new(FAKE_ZINGOD)
        .args(args)
        .output()
        .expect("run fake-zingod")
}

#[test]
fn reports_the_pinned_version() {
    let pin = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../ZINGOD_VERSION"))
        .expect("read ZINGOD_VERSION");
    for flag in ["--version", "-V"] {
        let out = run(&[flag]);
        assert!(out.status.success(), "{flag}: {out:?}");
        assert_eq!(
            String::from_utf8(out.stdout).expect("UTF-8"),
            format!("zingod {}\n", pin.trim()),
            "{flag}"
        );
    }
}

#[test]
fn generates_a_config_file() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("zingod.toml");
    let path = path.to_str().expect("UTF-8 path");
    for flag in ["-o", "--output"] {
        let out = run(&["generate-config", flag, path]);
        assert!(out.status.success(), "{flag}: {out:?}");
        let config = std::fs::read_to_string(path).expect("the generated file");
        assert!(config.contains("network = \"mainnet\"\n"), "{config}");
        std::fs::remove_file(path).expect("remove the generated file");
    }
}

#[test]
fn refuses_anything_else() {
    for args in [
        &[][..],
        &["start"],
        &["-c", "zingod.toml"],
        &["generate-config"],
        &["--version", "extra"],
    ] {
        let out = run(args);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {out:?}");
        assert!(out.stdout.is_empty(), "{args:?}: {out:?}");
    }
}

/// `start`, against fake-zebrad. zingod runs on Linux only, and these send `SIGTERM`.
#[cfg(unix)]
mod start {
    use std::{
        net::{Ipv4Addr, SocketAddr, TcpListener},
        path::{Path, PathBuf},
        process::{Command, Stdio},
        time::Duration,
    };

    use launcher_core::zingod::{ZingodConfig, ZingodNetwork};
    use tokio::{
        io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
        process::{Child, ChildStdout},
        time::timeout,
    };

    use super::FAKE_ZINGOD;

    const FAKE_ZEBRAD: &str = env!("CARGO_BIN_EXE_fake-zebrad");
    const T: Duration = Duration::from_secs(20);

    fn free_port() -> u16 {
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    /// The system's error `errno` as Rust prints it, as zingod's texts carry it: the numbers
    /// differ between Linux and macOS (`EADDRINUSE` is 98 on Linux, 48 on macOS).
    fn os_error(errno: nix::errno::Errno) -> String {
        std::io::Error::from_raw_os_error(errno as i32).to_string()
    }

    /// A fake-zebrad with its RPC server and cookie, in `mode`.
    struct Zebrad {
        child: Child,
        addr: SocketAddr,
        cookie: PathBuf,
        config: PathBuf,
        mode: String,
    }

    impl Zebrad {
        async fn spawn(dir: &Path, mode: &str, port: u16) -> Self {
            let cookie_dir = dir.join("rpc");
            let config = dir.join("zebrad.toml");
            std::fs::write(
                &config,
                format!(
                    "[rpc]\nlisten_addr = \"127.0.0.1:{port}\"\ncookie_dir = {}\n",
                    toml_edit::Value::from(cookie_dir.to_str().unwrap())
                ),
            )
            .unwrap();
            let (child, addr) = Self::start(&config, mode).await;
            Zebrad {
                child,
                addr,
                cookie: cookie_dir.join(".cookie"),
                config,
                mode: mode.to_string(),
            }
        }

        async fn start(config: &Path, mode: &str) -> (Child, SocketAddr) {
            let mut child = tokio::process::Command::new(FAKE_ZEBRAD)
                .arg("-c")
                .arg(config)
                .arg("start")
                .env("FAKE_ZEBRAD_MODE", mode)
                .env("FAKE_ZEBRAD_HEARTBEAT_MS", "60000")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
            let addr = timeout(T, async {
                loop {
                    let line = lines.next_line().await.unwrap().expect("the RPC line");
                    if let Some((_, addr)) = line.split_once("Opened RPC endpoint at ") {
                        return addr.trim().parse::<SocketAddr>().unwrap();
                    }
                }
            })
            .await
            .expect("fake-zebrad opened its RPC endpoint");
            tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });
            (child, addr)
        }

        /// Stops and starts it again on the same port: a new cookie.
        async fn restart(&mut self) {
            self.child.kill().await.unwrap();
            let (child, addr) = Self::start(&self.config, &self.mode).await;
            assert_eq!(addr, self.addr);
            self.child = child;
        }
    }

    /// fake-zingod started on Zeal's configuration, with its settings in `fake-zingod.env`.
    struct Zingod {
        child: Child,
        listen: SocketAddr,
        stdout: tokio::io::Lines<BufReader<ChildStdout>>,
    }

    /// Zeal's configuration, in `dir/<port>.toml` so each start has its own file.
    fn write_config(dir: &Path, validator: SocketAddr, cookie: &Path, port: u16) -> PathBuf {
        let config = ZingodConfig {
            network: ZingodNetwork::Testnet,
            validator,
            cookie: cookie.to_path_buf(),
            listen: SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
            index_dir: dir.join("zingod"),
        };
        let path = dir.join(format!("zingod-{port}.toml"));
        config.write(&path).unwrap();
        path
    }

    fn settings(dir: &Path, lines: &str) {
        std::fs::write(dir.join("fake-zingod.env"), lines).unwrap();
    }

    async fn start_zingod(config: &Path) -> Zingod {
        let text = std::fs::read_to_string(config).unwrap();
        let listen: SocketAddr = text
            .lines()
            .find_map(|line| line.strip_prefix("grpc_listen_address = "))
            .unwrap()
            .trim_matches('"')
            .parse()
            .unwrap();
        let mut child = tokio::process::Command::new(FAKE_ZINGOD)
            .arg("-c")
            .arg(config)
            .arg("start")
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap()).lines();
        timeout(T, async {
            loop {
                let line = stdout
                    .next_line()
                    .await
                    .unwrap()
                    .expect("the Listening line");
                if line.contains("message=Listening ") && line.ends_with("component=Grpc") {
                    return;
                }
            }
        })
        .await
        .expect("fake-zingod listens");
        Zingod {
            child,
            listen,
            stdout,
        }
    }

    /// One HTTP/1.1 request on a new connection: the status code and the body.
    async fn http(addr: SocketAddr, method: &str, path: &str) -> (u16, String) {
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(
                format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Length: 0\r\n\r\n")
                    .as_bytes(),
            )
            .await
            .unwrap();
        let mut answer = String::new();
        timeout(T, stream.read_to_string(&mut answer))
            .await
            .unwrap()
            .unwrap();
        let code = answer.split_whitespace().nth(1).unwrap().parse().unwrap();
        let body = answer.split_once("\r\n\r\n").unwrap().1.to_string();
        (code, body)
    }

    async fn status(addr: SocketAddr) -> serde_json::Value {
        let (code, body) = http(addr, "GET", "/status").await;
        assert_eq!(code, 200, "{body}");
        serde_json::from_str(&body).unwrap()
    }

    /// Waits until `/status` satisfies `predicate`.
    async fn status_until(
        addr: SocketAddr,
        what: &str,
        predicate: impl Fn(&serde_json::Value) -> bool,
    ) -> serde_json::Value {
        timeout(T, async {
            loop {
                let status = status(addr).await;
                if predicate(&status) {
                    return status;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("/status never showed {what}"))
    }

    async fn sigterm(child: &mut Child) -> std::process::ExitStatus {
        let pid = nix::unistd::Pid::from_raw(i32::try_from(child.id().unwrap()).unwrap());
        nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGTERM).unwrap();
        timeout(T, child.wait()).await.unwrap().unwrap()
    }

    #[tokio::test]
    async fn serves_status_and_stops_on_sigterm() {
        let dir = tempfile::tempdir().unwrap();
        let zebrad = Zebrad::spawn(dir.path(), "ok", 0).await;
        let config = write_config(dir.path(), zebrad.addr, &zebrad.cookie, free_port());
        settings(dir.path(), "FAKE_ZINGOD_POLL_MS=50\n");
        let mut zingod = start_zingod(&config).await;

        let serving = status_until(zingod.listen, "serving", |s| s["state"] == "serving").await;
        assert_eq!(serving["chain"]["network"], "testnet");
        assert_eq!(serving["chain"]["validators"]["answering"], 1);
        assert_eq!(serving["chain"]["served"], 2_500_000);
        assert_eq!(
            serving["build"]["version"],
            launcher_types::zingod::version()
        );
        let parsed: launcher_types::zingod::ZingodStatus =
            serde_json::from_value(serving).expect("zingod's shape");
        let parsed = launcher_types::zingod::IndexerStatus::from(parsed);
        assert_eq!(parsed.tip, Some(2_500_000));

        let (code, readyz) = http(zingod.listen, "GET", "/readyz").await;
        assert_eq!(code, 200, "{readyz}");
        let (code, _) = http(
            zingod.listen,
            "POST",
            "/cash.z.wallet.sdk.rpc.CompactTxStreamer/GetLightdInfo",
        )
        .await;
        assert_eq!(code, 404, "no gRPC method over HTTP/1.1");

        let exit = sigterm(&mut zingod.child).await;
        assert_eq!(exit.code(), Some(0));
        let mut rest = Vec::new();
        while let Ok(Some(line)) = zingod.stdout.next_line().await {
            rest.push(line);
        }
        assert!(
            rest.last()
                .is_some_and(|line| line.contains("message=\"Shutdown complete\"")),
            "{rest:?}"
        );
    }

    /// Each captured failure, from the real cause where the fake can make one.
    #[tokio::test]
    async fn fails_as_zingod_does() {
        let dir = tempfile::tempdir().unwrap();
        let zebrad = Zebrad::spawn(dir.path(), "ok", 0).await;
        let port = free_port();
        let config = write_config(dir.path(), zebrad.addr, &zebrad.cookie, port);
        let mut first = start_zingod(&config).await;
        let failure = |config: &Path| {
            let out = Command::new(FAKE_ZINGOD)
                .arg("-c")
                .arg(config)
                .arg("start")
                .env_clear()
                .output()
                .unwrap();
            assert_eq!(out.status.code(), Some(1), "{out:?}");
            String::from_utf8(out.stderr).unwrap()
        };

        assert_eq!(
            failure(&config),
            format!(
                "Error: binding the gRPC listener: {}\n",
                os_error(nix::errno::Errno::EADDRINUSE)
            )
        );
        let other = write_config(dir.path(), zebrad.addr, &zebrad.cookie, free_port());
        let locked = failure(&other);
        assert!(
            locked.starts_with("Error: manifest io: ")
                && locked.ends_with("/indexes/utxo_set/LOCK is held by another process\n"),
            "{locked}"
        );
        sigterm(&mut first.child).await;

        let unknown = dir.path().join("unknown.toml");
        std::fs::write(
            &unknown,
            format!("bogus = 1\n{}", std::fs::read_to_string(&config).unwrap()),
        )
        .unwrap();
        assert_eq!(
            failure(&unknown).trim_end(),
            include_str!("fixtures/zingod-1.1.0-config-error.stderr.log").trim_end()
        );

        let folder = dir.path().join("cookie-folder");
        std::fs::create_dir_all(folder.join(".cookie")).unwrap();
        let cookie = write_config(
            dir.path(),
            zebrad.addr,
            &folder.join(".cookie"),
            free_port(),
        );
        assert_eq!(
            failure(&cookie),
            format!(
                "Error: cannot read validator cookie {}: {}\n",
                folder.join(".cookie").display(),
                os_error(nix::errno::Errno::EISDIR)
            )
        );

        for (mode, text) in [
            ("index-broken", "Error: MANIFEST checksum mismatch\n"),
            (
                "permission-denied",
                "Error: manifest io: Permission denied (os error 13)\n",
            ),
        ] {
            settings(dir.path(), &format!("FAKE_ZINGOD_MODE={mode}\n"));
            assert_eq!(failure(&config), text, "{mode}");
        }
    }

    /// zingod reads its cookie once and again only after a 401: a zebrad that answers 401 after
    /// its restart gets it back, one that closes the connection never does.
    #[tokio::test]
    async fn rereads_the_cookie_only_after_a_401() {
        for (mode, recovers) in [("ok", true), ("close-on-wrong-cookie", false)] {
            let dir = tempfile::tempdir().unwrap();
            let mut zebrad = Zebrad::spawn(dir.path(), mode, free_port()).await;
            let config = write_config(dir.path(), zebrad.addr, &zebrad.cookie, free_port());
            settings(dir.path(), "FAKE_ZINGOD_POLL_MS=50\n");
            let zingod = start_zingod(&config).await;
            let answering = |s: &serde_json::Value| s["chain"]["validators"]["answering"] == 1;
            status_until(zingod.listen, "a validator answering", answering).await;

            zebrad.restart().await;
            if recovers {
                status_until(zingod.listen, "the validator back", answering).await;
            } else {
                status_until(zingod.listen, "no validator answering", |s| !answering(s)).await;
                tokio::time::sleep(Duration::from_millis(500)).await;
                assert!(
                    !answering(&status(zingod.listen).await),
                    "{mode}: the stale cookie never works again"
                );
            }
        }
    }
}
