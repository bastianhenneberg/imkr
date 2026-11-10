//! Nobody who installs imkr may see every machine of the NetBird network
//! (#9648): that network is shared with other people's machines. imkr
//! never runs `netbird`, so its peers show up nowhere, while the
//! user's own tailnet still does.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

fn script(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn a_netbird_network_with_foreign_peers_is_listed_nowhere() {
    let world = std::env::temp_dir().join(format!("imkr-no-netbird-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&world);
    let bin = world.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let asked = world.join("netbird-asked");
    script(
        &bin.join("netbird"),
        &format!(
            r#"#!/bin/sh
touch '{}'
cat <<'EOF'
{{"fqdn": "laptop.netbird.selfhosted", "peers": {{"total": 2, "connected": 2, "details": [
  {{"fqdn": "colleague-pc.netbird.selfhosted", "netbirdIp": "100.97.10.1", "status": "Connected"}},
  {{"fqdn": "fail2ban-server.netbird.selfhosted", "netbirdIp": "100.97.10.2", "status": "Idle"}}
]}}}}
EOF
"#,
            asked.display()
        ),
    );
    script(
        &bin.join("tailscale"),
        r#"#!/bin/sh
cat <<'EOF'
{"Self": {"HostName": "laptop", "DNSName": "laptop.tail1.ts.net.", "OS": "linux"},
 "Peer": {"a": {"HostName": "desk", "DNSName": "desk.tail1.ts.net.", "OS": "linux", "Online": true}}}
EOF
"#,
    );
    script(&bin.join("notify-send"), "#!/bin/sh\nexit 0\n");
    let home = world.join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(world.join("config.toml"), "notifications = \"off\"\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_imkr"))
        .args(["hosts", "vpn"])
        .env_clear()
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("HOME", &home)
        .env("XDG_STATE_HOME", world.join("state"))
        .env("XDG_CONFIG_HOME", world.join("xdg-config"))
        .env("XDG_RUNTIME_DIR", world.join("run"))
        .env("IMKR_CONFIG", world.join("config.toml"))
        .env("IMKR_HOSTNAME", "laptop")
        .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout).into_owned()
        + &String::from_utf8_lossy(&output.stderr);
    let ran_netbird = asked.exists();
    let _ = std::fs::remove_dir_all(&world);
    assert!(output.status.success(), "{text}");
    assert!(text.contains("desk"), "the tailnet is still listed: {text}");
    for foreign in ["colleague-pc", "fail2ban-server", "100.97.10"] {
        assert!(!text.contains(foreign), "{foreign} listed: {text}");
    }
    assert!(!ran_netbird, "imkr ran netbird");
}
