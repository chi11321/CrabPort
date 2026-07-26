//! Integration-style tests for the Store: real SQLite DB in a temp dir,
//! real encryption key — only the filesystem location is faked.
//!
//! Each test opens its own store in a unique temp directory (cleaned up on
//! drop) so tests can run in parallel without interfering.

use std::path::PathBuf;

use crate::credential::{
    CredentialEntry, CredentialKind, GroupKind, HostEntry, HostKind, PrivateKeyKind, ProxyEntry,
    ProxyKind, TunnelEntry, TunnelKind,
};

use super::Store;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// A Store in a unique temp dir, removed on drop (even if the test's other
/// assertions already passed — a panic leaks the dir until the next run
/// reuses and clears the same path).
struct TempStore {
    store: Store,
    dir: PathBuf,
}

impl TempStore {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "crabport-store-test-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::open_at(dir.clone()).expect("open test store");
        Self { store, dir }
    }
}

impl Drop for TempStore {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl std::ops::Deref for TempStore {
    type Target = Store;
    fn deref(&self) -> &Store {
        &self.store
    }
}

/// Minimal SSH host with `name`; tweak fields per test via struct update.
fn host(name: &str) -> HostEntry {
    HostEntry {
        id: 0,
        name: name.into(),
        host: "example.com".into(),
        port: 22,
        username: "root".into(),
        credential_id: None,
        kind: HostKind::Ssh,
        last_login: None,
        favorite: false,
        proxy_id: None,
        group_id: None,
        startup_command: String::new(),
        serial_baud_rate: None,
        serial_data_bits: None,
        serial_parity: None,
        serial_stop_bits: None,
        serial_flow_control: None,
        jump_host_id: None,
    }
}

fn credential(name: &str, secret: &str) -> CredentialEntry {
    CredentialEntry {
        id: 0,
        name: name.into(),
        kind: CredentialKind::Password,
        anonymous: false,
        secret: secret.into(),
        private_key: String::new(),
        private_key_kind: PrivateKeyKind::Content,
        public_key: String::new(),
        certificate: String::new(),
    }
}

fn tunnel(name: &str, host_id: i64) -> TunnelEntry {
    TunnelEntry {
        id: 0,
        name: name.into(),
        host_id,
        kind: TunnelKind::Local,
        bind_addr: "127.0.0.1".into(),
        bind_port: 8080,
        target_host: "10.0.0.5".into(),
        target_port: 80,
        created_at: 0,
        favorite: false,
        group_id: None,
    }
}

// ---------------------------------------------------------------------------
// Hosts
// ---------------------------------------------------------------------------

#[test]
fn host_crud_roundtrip_all_fields() {
    let s = TempStore::new("host-crud");

    // Serial host exercises every optional column at once.
    let mut h = host("rack-console");
    h.kind = HostKind::Serial;
    h.port = 0;
    h.startup_command = "dmesg | tail\n".into();
    h.serial_baud_rate = Some(115_200);
    h.serial_data_bits = Some(8);
    h.serial_parity = Some("even".into());
    h.serial_stop_bits = Some(2);
    h.serial_flow_control = Some("hardware".into());

    let id = s.add_host(&h).unwrap();
    let got = s.find_host(id).unwrap().expect("host exists");
    assert_eq!(got.name, "rack-console");
    assert_eq!(got.kind, HostKind::Serial);
    assert_eq!(got.startup_command, "dmesg | tail\n");
    assert_eq!(got.serial_baud_rate, Some(115_200));
    assert_eq!(got.serial_data_bits, Some(8));
    assert_eq!(got.serial_parity.as_deref(), Some("even"));
    assert_eq!(got.serial_stop_bits, Some(2));
    assert_eq!(got.serial_flow_control.as_deref(), Some("hardware"));

    // Update flips the kind and clears serial config.
    let mut updated = got.clone();
    updated.kind = HostKind::Telnet;
    updated.port = 23;
    updated.serial_baud_rate = None;
    updated.serial_parity = None;
    s.update_host(&updated).unwrap();
    let got = s.find_host(id).unwrap().unwrap();
    assert_eq!(got.kind, HostKind::Telnet);
    assert_eq!(got.port, 23);
    assert_eq!(got.serial_baud_rate, None);
    assert_eq!(got.serial_parity, None);

    s.remove_host(id).unwrap();
    assert!(s.find_host(id).unwrap().is_none());
    assert!(s.hosts().unwrap().is_empty());
}

#[test]
fn hosts_order_favorites_then_recent_login() {
    let s = TempStore::new("host-order");
    let a = s.add_host(&host("a")).unwrap();
    let b = s.add_host(&host("b")).unwrap();
    let c = s.add_host(&host("c")).unwrap();

    // c logged in most recently, b is a favorite → b first (favorite DESC
    // wins over last_login), then c (recent login), then a.
    s.touch_host_login(c).unwrap();
    assert!(s.toggle_host_favorite(b).unwrap());

    let names: Vec<String> = s.hosts().unwrap().into_iter().map(|h| h.name).collect();
    assert_eq!(names, ["b", "c", "a"]);

    // Toggling back demotes b behind c again.
    assert!(!s.toggle_host_favorite(b).unwrap());
    let names: Vec<String> = s.hosts().unwrap().into_iter().map(|h| h.name).collect();
    assert_eq!(names[0], "c");

    // Unknown id is an error, not a silent no-op.
    assert!(s.toggle_host_favorite(a + b + c + 999).is_err());
}

#[test]
fn deleting_jump_host_degrades_dependents_to_direct() {
    let s = TempStore::new("jump-set-null");
    let bastion = s.add_host(&host("bastion")).unwrap();
    let mut inner = host("inner");
    inner.jump_host_id = Some(bastion);
    let inner_id = s.add_host(&inner).unwrap();
    assert_eq!(
        s.find_host(inner_id).unwrap().unwrap().jump_host_id,
        Some(bastion)
    );

    // ON DELETE SET NULL: removing the bastion must degrade `inner` to a
    // direct connection, not delete it and not fail.
    s.remove_host(bastion).unwrap();
    let inner = s.find_host(inner_id).unwrap().expect("dependent survives");
    assert_eq!(inner.jump_host_id, None);
}

// ---------------------------------------------------------------------------
// Credentials
// ---------------------------------------------------------------------------

#[test]
fn credential_roundtrip_and_encrypted_at_rest() {
    let s = TempStore::new("cred-crypt");
    let mut c = credential("prod-login", "s3cr3t-pa55");
    c.kind = CredentialKind::Certificate;
    c.private_key = "-----BEGIN OPENSSH PRIVATE KEY-----\nabc\n-----END-----".into();
    c.private_key_kind = PrivateKeyKind::Path;
    c.public_key = "ssh-ed25519 AAAA".into();
    c.certificate = "cert-data".into();

    let id = s.add_credential(&c).unwrap();
    let got = s.find_credential(id).unwrap().expect("credential exists");
    assert_eq!(got.secret, "s3cr3t-pa55");
    assert_eq!(got.private_key, c.private_key);
    assert_eq!(got.private_key_kind, PrivateKeyKind::Path);
    assert_eq!(got.public_key, "ssh-ed25519 AAAA");
    assert_eq!(got.certificate, "cert-data");
    assert_eq!(got.kind, CredentialKind::Certificate);

    // At-rest check: the raw secret BLOB in SQLite must not contain the
    // plaintext bytes (AES-256-GCM ciphertext + nonce).
    let raw: Vec<u8> = s
        .db
        .query_row(
            "SELECT secret FROM credentials WHERE id = ?1",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!raw.is_empty());
    assert!(
        !raw.windows(b"s3cr3t-pa55".len()).any(|w| w == b"s3cr3t-pa55"),
        "secret stored in plaintext"
    );

    // Empty secrets stay empty (no ciphertext for nothing).
    let empty_id = s.add_credential(&credential("empty", "")).unwrap();
    assert_eq!(s.find_credential(empty_id).unwrap().unwrap().secret, "");

    s.remove_credential(id).unwrap();
    assert!(s.find_credential(id).unwrap().is_none());
}

#[test]
fn resolve_secret_follows_host_credential_link() {
    let s = TempStore::new("cred-resolve");
    let cred_id = s.add_credential(&credential("login", "hunter2")).unwrap();

    let mut h = host("app");
    h.credential_id = Some(cred_id);
    let host_id = s.add_host(&h).unwrap();
    let h = s.find_host(host_id).unwrap().unwrap();
    assert_eq!(s.resolve_secret(&h).unwrap().as_deref(), Some("hunter2"));

    // Host without credential → None.
    let bare_id = s.add_host(&host("bare")).unwrap();
    let bare = s.find_host(bare_id).unwrap().unwrap();
    assert_eq!(s.resolve_secret(&bare).unwrap(), None);

    // Dangling credential_id (credential deleted) → None, not an error.
    s.remove_credential(cred_id).unwrap();
    let h = s.find_host(host_id).unwrap().unwrap();
    assert_eq!(s.resolve_secret(&h).unwrap(), None);
}

// ---------------------------------------------------------------------------
// Proxies
// ---------------------------------------------------------------------------

#[test]
fn proxy_roundtrip_password_encrypted_and_config_decrypts() {
    let s = TempStore::new("proxy-crud");
    let p = ProxyEntry {
        id: 0,
        name: "corp".into(),
        kind: ProxyKind::Socks5,
        host: "proxy.corp".into(),
        port: 1080,
        username: Some("alice".into()),
        password: Some(b"pw-plain".to_vec()),
        created_at: 1,
    };
    let id = s.add_proxy(&p).unwrap();

    // find_proxy returns the encrypted blob, not the plaintext.
    let row = s.find_proxy(id).unwrap().expect("proxy exists");
    assert_eq!(row.kind, ProxyKind::Socks5);
    assert_eq!(row.username.as_deref(), Some("alice"));
    let stored = row.password.as_deref().expect("password stored");
    assert_ne!(stored, b"pw-plain", "proxy password stored in plaintext");

    // find_proxy_config decrypts back to the original.
    let cfg = s.find_proxy_config(id).unwrap().unwrap();
    assert_eq!(cfg.password.as_deref(), Some("pw-plain"));
    assert!(cfg.is_enabled());

    // Empty username round-trips as None (normalized on read).
    let anon = ProxyEntry {
        username: None,
        password: None,
        name: "open".into(),
        ..p.clone()
    };
    let anon_id = s.add_proxy(&anon).unwrap();
    let row = s.find_proxy(anon_id).unwrap().unwrap();
    assert_eq!(row.username, None);
    assert_eq!(row.password, None);

    s.remove_proxy(id).unwrap();
    assert!(s.find_proxy(id).unwrap().is_none());
    assert_eq!(s.proxies().unwrap().len(), 1);
}

#[test]
fn deleting_proxy_clears_host_references() {
    let s = TempStore::new("proxy-set-null");
    let proxy_id = s
        .add_proxy(&ProxyEntry {
            id: 0,
            name: "p".into(),
            kind: ProxyKind::Http,
            host: "proxy".into(),
            port: 8080,
            username: None,
            password: None,
            created_at: 0,
        })
        .unwrap();
    let mut h = host("behind-proxy");
    h.proxy_id = Some(proxy_id);
    let host_id = s.add_host(&h).unwrap();

    // Documented behavior: removing a proxy degrades referencing hosts to
    // a direct connection (proxy_id NULL) instead of failing or cascading.
    s.remove_proxy(proxy_id).unwrap();
    let h = s.find_host(host_id).unwrap().expect("host survives");
    assert_eq!(h.proxy_id, None);
}

// ---------------------------------------------------------------------------
// Groups
// ---------------------------------------------------------------------------

#[test]
fn groups_are_scoped_by_kind_and_auto_ordered() {
    let s = TempStore::new("groups");
    let g1 = s.add_group("Production", GroupKind::Host, None).unwrap();
    let g2 = s.add_group("Staging", GroupKind::Host, None).unwrap();
    // Same name under a different kind is a distinct group.
    let g3 = s.add_group("Production", GroupKind::Snippet, None).unwrap();
    assert_ne!(g1, g3);

    let hosts_groups = s.groups(GroupKind::Host).unwrap();
    assert_eq!(hosts_groups.len(), 2);
    // Auto sort_order appends: Production=0, Staging=1.
    assert_eq!(hosts_groups[0].name, "Production");
    assert_eq!(hosts_groups[0].sort_order, 0);
    assert_eq!(hosts_groups[1].sort_order, 1);
    assert_eq!(s.groups(GroupKind::Snippet).unwrap().len(), 1);
    assert_eq!(s.groups(GroupKind::Tunnel).unwrap().len(), 0);

    // Favorite pins above sort_order.
    s.toggle_group_favorite(g2).unwrap();
    let hosts_groups = s.groups(GroupKind::Host).unwrap();
    assert_eq!(hosts_groups[0].name, "Staging");

    // Rename.
    s.update_group(g1, "Prod", None).unwrap();
    assert_eq!(s.find_group(g1).unwrap().unwrap().name, "Prod");
}

#[test]
fn deleting_group_ungroups_members() {
    let s = TempStore::new("group-set-null");
    let gid = s.add_group("Team", GroupKind::Host, None).unwrap();
    let mut h = host("grouped");
    h.group_id = Some(gid);
    let host_id = s.add_host(&h).unwrap();

    s.remove_group(gid).unwrap();
    // ON DELETE SET NULL: member falls back to "ungrouped".
    assert_eq!(s.find_host(host_id).unwrap().unwrap().group_id, None);
    assert!(s.find_group(gid).unwrap().is_none());
}

// ---------------------------------------------------------------------------
// Snippets
// ---------------------------------------------------------------------------

#[test]
fn snippet_crud_and_ordering() {
    let s = TempStore::new("snippets");
    // Empty name falls back to the command text.
    let a = s.add_snippet("  ", "ls -la", false, None).unwrap();
    let b = s.add_snippet("tail logs", "tail -f /var/log/syslog", false, None).unwrap();
    let list = s.snippets().unwrap();
    assert_eq!(list.len(), 2);
    // Newest first (id DESC) among non-favorites.
    assert_eq!(list[0].id, b);
    assert_eq!(list[1].name, "ls -la");

    // Favorite floats above newer entries.
    assert!(s.toggle_snippet_favorite(a).unwrap());
    let list = s.snippets().unwrap();
    assert_eq!(list[0].id, a);
    assert!(list[0].favorite);

    // Update rewrites name/command and empty name falls back again.
    s.update_snippet(b, "", "htop", true, None).unwrap();
    let updated = s.snippets().unwrap().into_iter().find(|x| x.id == b).unwrap();
    assert_eq!(updated.name, "htop");
    assert_eq!(updated.command, "htop");
    assert!(updated.favorite);

    s.remove_snippet(a).unwrap();
    assert_eq!(s.snippets().unwrap().len(), 1);
}

// ---------------------------------------------------------------------------
// Tunnels
// ---------------------------------------------------------------------------

#[test]
fn tunnel_crud_scoped_to_host_and_cascade_on_host_delete() {
    let s = TempStore::new("tunnels");
    let h1 = s.add_host(&host("h1")).unwrap();
    let h2 = s.add_host(&host("h2")).unwrap();

    let t1 = s.add_tunnel(&tunnel("web", h1)).unwrap();
    let _t2 = s.add_tunnel(&tunnel("db", h1)).unwrap();
    let t3 = s.add_tunnel(&tunnel("other", h2)).unwrap();

    assert_eq!(s.tunnels().unwrap().len(), 3);
    assert_eq!(s.tunnels_for_host(h1).unwrap().len(), 2);
    assert_eq!(s.tunnels_for_host(h2).unwrap().len(), 1);

    // Empty bind_addr is coerced to the loopback default on insert.
    let mut open = tunnel("open", h2);
    open.bind_addr = String::new();
    open.kind = TunnelKind::Dynamic;
    let open_id = s.add_tunnel(&open).unwrap();
    let got = s.find_tunnel(open_id).unwrap().unwrap();
    assert_eq!(got.bind_addr, "127.0.0.1");
    assert_eq!(got.kind, TunnelKind::Dynamic);

    // Favorite ordering within tunnels().
    assert!(s.toggle_tunnel_favorite(t3).unwrap());
    assert_eq!(s.tunnels().unwrap()[0].id, t3);

    // Update round-trip.
    let mut upd = s.find_tunnel(t1).unwrap().unwrap();
    upd.kind = TunnelKind::Remote;
    upd.bind_port = 9000;
    s.update_tunnel(&upd).unwrap();
    let got = s.find_tunnel(t1).unwrap().unwrap();
    assert_eq!(got.kind, TunnelKind::Remote);
    assert_eq!(got.bind_port, 9000);

    // ON DELETE CASCADE: removing the host removes its tunnels only.
    s.remove_host(h1).unwrap();
    assert_eq!(s.tunnels_for_host(h1).unwrap().len(), 0);
    assert_eq!(s.tunnels().unwrap().len(), 2);
}

// ---------------------------------------------------------------------------
// Command history
// ---------------------------------------------------------------------------

#[test]
fn command_history_dedups_and_promotes_reruns() {
    let s = TempStore::new("cmd-history");
    let h = s.add_host(&host("h")).unwrap();

    s.add_command(h, "ls").unwrap();
    s.add_command(h, "pwd").unwrap();
    // Backdate `ls` so ordering is deterministic (add_command stamps
    // whole-second timestamps, which tie inside a fast test).
    s.db
        .execute(
            "UPDATE command_history SET updated_at = updated_at - 10 WHERE command = 'ls'",
            [],
        )
        .unwrap();
    assert_eq!(s.commands_for_host(h).unwrap(), ["pwd", "ls"]);

    // Re-running `ls` promotes it (updated_at bumped to now) without
    // inserting a duplicate row.
    s.add_command(h, "ls").unwrap();
    s.db
        .execute(
            "UPDATE command_history SET updated_at = updated_at - 10 WHERE command = 'pwd'",
            [],
        )
        .unwrap();
    assert_eq!(s.commands_for_host(h).unwrap(), ["ls", "pwd"]);

    // History is per-host.
    let other = s.add_host(&host("other")).unwrap();
    assert!(s.commands_for_host(other).unwrap().is_empty());
}

#[test]
fn command_history_evicts_lru_beyond_cap() {
    let s = TempStore::new("cmd-lru");
    let h = s.add_host(&host("h")).unwrap();

    // Insert one over the cap; backdate the first command so it's the
    // deterministic LRU victim despite same-second timestamps.
    s.add_command(h, "victim").unwrap();
    s.db
        .execute(
            "UPDATE command_history SET updated_at = updated_at - 100, created_at = created_at - 100",
            [],
        )
        .unwrap();
    for i in 0..300 {
        s.add_command(h, &format!("cmd-{i}")).unwrap();
    }

    let cmds = s.commands_for_host(h).unwrap();
    assert_eq!(cmds.len(), 300, "history must be capped at 300 per host");
    assert!(
        !cmds.iter().any(|c| c == "victim"),
        "oldest (LRU) entry must be evicted"
    );
}
