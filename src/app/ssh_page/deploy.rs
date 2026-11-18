use super::*;
use sha2::{Digest, Sha256};
use std::io::Read as _;
use std::net::SocketAddr;

const ROOT: &str = r#"root="${XDG_DATA_HOME:-$HOME/.local/share}/fintwind-ssh""#;
const MAX_BINARY: u64 = 256 * 1024 * 1024;

#[derive(Clone, Copy)]
pub(super) enum LinuxTarget {
    X86_64,
    Aarch64,
}

impl LinuxTarget {
    pub(super) fn from_system(system: &str) -> Option<Self> {
        match system {
            "Linux x86_64" => Some(Self::X86_64),
            "Linux aarch64" => Some(Self::Aarch64),
            _ => None,
        }
    }

    pub(super) fn asset(self) -> &'static str {
        match self {
            Self::X86_64 => "fintwind-daemon-linux-x86_64",
            Self::Aarch64 => "fintwind-daemon-linux-aarch64",
        }
    }

    fn machine(self) -> u16 {
        match self {
            Self::X86_64 => 62,
            Self::Aarch64 => 183,
        }
    }
}

pub(super) enum Detection {
    Missing,
    Stopped,
    Running { port: u16, token_file: String },
}

pub(super) async fn detect(session: &client::Handle<HostKey>) -> Result<Detection> {
    let output = command(
        session,
        r#"
if [ ! -x "$root/daemon" ]; then
    printf 'missing\n'
else
    flock -n -E 42 "$root/lock" true
    case $? in
        0) printf 'stopped\n'; exit 0 ;;
        42) ;;
        *) exit 1 ;;
    esac
    IFS= read -r ready < "$root/ready" || { printf 'stopped\n'; exit 0; }
    printf '%s\n%s\n' "$root/token" "$ready"
fi
"#,
        None,
    )
    .await?;
    match output.trim_end() {
        "missing" => Ok(Detection::Missing),
        "stopped" => Ok(Detection::Stopped),
        output => {
            let (token_file, ready) = output
                .split_once('\n')
                .context("invalid remote daemon state")?;
            ensure!(
                token_file.starts_with('/'),
                "invalid remote daemon token path"
            );
            let ready: fintwind_protocol::DaemonReady = serde_json::from_str(ready)?;
            let address: SocketAddr = ready.address.parse()?;
            ensure!(
                address.ip() == std::net::Ipv4Addr::LOCALHOST && address.port() != 0,
                "invalid remote daemon address"
            );
            Ok(Detection::Running {
                port: address.port(),
                token_file: token_file.to_owned(),
            })
        }
    }
}

async fn start(session: &client::Handle<HostKey>) -> Result<(u16, String)> {
    if let Detection::Running { port, token_file } = detect(session).await? {
        return Ok((port, token_file));
    }
    tokio::time::timeout(Duration::from_secs(180), async {
            command(session, r#"
umask 077
test -x "$root/daemon" || exit 1
command -v flock >/dev/null || exit 1
nohup flock -n -o "$root/lock" sh -c '
    set -e
    cd "$1"
    : > ready
    if [ ! -s token ]; then
        od -An -N32 -tx1 /dev/urandom | tr -d " \n" > token.new
        test "$(wc -c < token.new)" -eq 64
        mv -f -- token.new token
    fi
    FINTWIND_DAEMON_TOKEN=$(cat token) exec ./daemon --bind 127.0.0.1:0 --data-dir "$PWD/data" > ready
' sh "$root" </dev/null >>"$root/daemon.log" 2>&1 &
"#, None).await?;
            for _ in 0..40 {
                if let Detection::Running { port, token_file } = detect(session).await? {
                    return Ok((port, token_file));
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            anyhow::bail!("{}", tr!("ssh.deploy_start_failed"));
    }).await.context(tr!("ssh.timeout"))?
}

pub(super) async fn ensure_current(
    session: &client::Handle<HostKey>,
    target: LinuxTarget,
) -> Result<(u16, String)> {
    let executable = std::env::current_exe()?;
    let mut path = executable.with_file_name(target.asset());
    if !path.is_file()
        && cfg!(target_os = "linux")
        && LinuxTarget::from_system(&format!("Linux {}", std::env::consts::ARCH))
            .is_some_and(|local| local.machine() == target.machine())
    {
        path = executable.with_file_name("fintwind-daemon");
    }
    if !path.is_file() {
        match detect(session).await? {
            Detection::Running { port, token_file } => return Ok((port, token_file)),
            Detection::Stopped => return start(session).await,
            Detection::Missing => {}
        }
    }
    let binary = read_binary(target, path)?;
    let digest = format!("{:x}", Sha256::digest(&binary));
    let version = env!("CARGO_PKG_VERSION");
    let remote = command(
        session,
        r#"
if [ -f "$root/version" ]; then
    sed -n '1p' "$root/version"
else
    printf '\n'
fi
if [ -x "$root/daemon" ]; then
    sha256sum "$root/daemon" | cut -d ' ' -f 1
    flock -n -E 42 "$root/lock" true
    case $? in
        0) printf 'stopped\n' ;;
        42)
            pid=$(sed -n 's/.*"pid":[[:space:]]*\([0-9][0-9]*\).*/\1/p' "$root/ready")
            case "$pid" in ''|*[!0-9]*) exit 1 ;; esac
            sha256sum "/proc/$pid/exe" | cut -d ' ' -f 1
            ;;
        *) exit 1 ;;
    esac
fi
"#,
        None,
    )
    .await?;
    let mut state = remote.lines();
    let remote_version = state.next();
    let installed = state.next();
    let running = state.next();
    if installed != Some(digest.as_str())
        || running.is_some_and(|hash| hash != "stopped" && hash != digest)
    {
        let script = format!(
            r#"
umask 077
mkdir -p -- "$root" || exit 1
exec 9>"$root/update.lock"
flock -w 180 9 || exit 1
tmp=$(mktemp "$root/upload.XXXXXXXX") || exit 1
trap 'rm -f -- "$tmp"' EXIT
cat > "$tmp" && chmod 700 "$tmp" || exit 1
[ "$(wc -c < "$tmp")" -eq "$1" ] || exit 1
[ "$(sha256sum "$tmp" | cut -d ' ' -f 1)" = '{digest}' ] || exit 1
flock -n -E 42 "$root/lock" true
case $? in
    0)
        if [ -x "$root/daemon" ] && [ "$(sha256sum "$root/daemon" | cut -d ' ' -f 1)" = '{digest}' ]; then
            exit 0
        fi
        ;;
    42)
        pid=$(sed -n 's/.*"pid":[[:space:]]*\([0-9][0-9]*\).*/\1/p' "$root/ready")
        case "$pid" in ''|*[!0-9]*) exit 1 ;; esac
        if [ -x "$root/daemon" ] && [ "$(sha256sum "$root/daemon" | cut -d ' ' -f 1)" = '{digest}' ] && [ "$(sha256sum "/proc/$pid/exe" | cut -d ' ' -f 1)" = '{digest}' ]; then
            exit 0
        fi
        [ "/proc/$pid/exe" -ef "$root/daemon" ] || exit 1
        kill -TERM "$pid" || exit 1
        flock -w 20 "$root/lock" true || exit 1
        ;;
    *) exit 1 ;;
esac
mv -f -- "$tmp" "$root/daemon" || exit 1
printf '%s\n%s\n' '{version}' '{digest}' > "$root/version"
"#
        );
        tokio::time::timeout(
            Duration::from_secs(180),
            command(session, &script, Some(&binary)),
        )
        .await
        .context(tr!("ssh.timeout"))??;
    } else if remote_version != Some(version) {
        let script = format!("printf '%s\\n%s\\n' '{version}' '{digest}' > \"$root/version\"");
        command(session, &script, None).await?;
    }
    start(session).await
}

fn read_binary(target: LinuxTarget, path: PathBuf) -> Result<Vec<u8>> {
    let mut file = std::fs::File::open(path)
        .with_context(|| tr!("ssh.deploy_asset_missing", asset = target.asset()))?;
    ensure!(
        file.metadata()?.len() <= MAX_BINARY,
        "Linux daemon binary is too large"
    );
    let mut header = [0; 20];
    file.read_exact(&mut header)?;
    ensure!(
        header[..6] == *b"\x7fELF\x02\x01"
            && u16::from_le_bytes([header[18], header[19]]) == target.machine(),
        "{}",
        tr!("ssh.deploy_invalid", asset = target.asset())
    );
    let mut bytes = header.to_vec();
    file.take(MAX_BINARY - 19).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_BINARY,
        "Linux daemon binary is too large"
    );
    Ok(bytes)
}

async fn command(
    session: &client::Handle<HostKey>,
    script: &str,
    binary: Option<&[u8]>,
) -> Result<String> {
    let mut channel = session.channel_open_session().await?;
    let size = binary.map_or(0, |bytes| bytes.len());
    let script = format!("{ROOT}\nset -- {size}\n{script}");
    channel.exec(true, script).await?;
    if let Some(binary) = binary {
        channel.data(binary).await?;
    }
    channel.eof().await?;
    let mut output = Vec::new();
    let mut error = Vec::new();
    let mut status = None;
    while let Some(message) = channel.wait().await {
        match message {
            russh::ChannelMsg::Data { data } => output.extend_from_slice(&data),
            russh::ChannelMsg::ExtendedData { data, .. } => error.extend_from_slice(&data),
            russh::ChannelMsg::ExitStatus { exit_status } => status = Some(exit_status),
            _ => {}
        }
        ensure!(
            output.len() + error.len() <= 8192,
            "SSH daemon response too large"
        );
    }
    ensure!(
        status == Some(0),
        "{}: {}",
        tr!("ssh.deploy_failed"),
        String::from_utf8_lossy(&error).trim()
    );
    Ok(String::from_utf8(output)?)
}
