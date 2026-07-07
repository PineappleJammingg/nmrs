# Builders Module

The `builders` module provides low-level APIs for constructing NetworkManager connection settings. Most users should use the high-level `NetworkManager` API instead — these builders are for advanced use cases where you need fine-grained control over the settings dictionary before calling NetworkManager D-Bus methods directly.

To submit builder output, use [`NetworkManager::dbus_connection()`](./network-manager.md#advanced-d-bus-access) together with [`nmrs::raw`](./raw.md) (`zbus` / `zvariant` re-exports). See [Submitting Builder Output](#submitting-builder-output) below.

## ConnectionBuilder

The base builder for all connection types. Handles common sections: `connection`, `ipv4`, `ipv6`.

```rust
use nmrs::builders::ConnectionBuilder;

let settings = ConnectionBuilder::new("802-3-ethernet", "MyConnection")
    .autoconnect(true)
    .autoconnect_priority(10)
    .ipv4_auto()
    .ipv6_auto()
    .build();
```

### Methods

| Method | Description |
|--------|-------------|
| `new(type, id)` | Create with connection type and name |
| `uuid(uuid)` | Set specific UUID |
| `interface_name(name)` | Restrict to a specific interface |
| `autoconnect(bool)` | Enable/disable auto-connect |
| `autoconnect_priority(i32)` | Set priority (higher = preferred) |
| `autoconnect_retries(i32)` | Set retry limit |
| `options(&ConnectionOptions)` | Apply options struct |
| `ipv4_auto()` | DHCP for IPv4 |
| `ipv4_manual(Vec<IpConfig>)` | Static IPv4 addresses |
| `ipv4_disabled()` | Disable IPv4 |
| `ipv4_link_local()` | Link-local IPv4 (169.254.x.x) |
| `ipv4_shared()` | Internet connection sharing |
| `ipv4_dns(Vec<Ipv4Addr>)` | Set DNS servers |
| `ipv4_gateway(Ipv4Addr)` | Set gateway |
| `ipv4_routes(Vec<Route>)` | Add static routes |
| `ipv6_auto()` | SLAAC/DHCPv6 |
| `ipv6_manual(Vec<IpConfig>)` | Static IPv6 addresses |
| `ipv6_ignore()` | Disable IPv6 |
| `ipv6_link_local()` | Link-local IPv6 only |
| `ipv6_dns(Vec<Ipv6Addr>)` | Set IPv6 DNS |
| `ipv6_gateway(Ipv6Addr)` | Set IPv6 gateway |
| `ipv6_routes(Vec<Route>)` | Add IPv6 static routes |
| `with_section(name, HashMap)` | Add custom settings section |
| `update_section(name, closure)` | Modify existing section |
| `build()` | Produce the settings dictionary |

### IpConfig

```rust
use nmrs::builders::IpConfig;

let ip = IpConfig::new("192.168.1.100", 24);
```

### Route

```rust
use nmrs::builders::Route;

let route = Route::new("10.0.0.0", 8)
    .next_hop("192.168.1.1")
    .metric(100);
```

## WifiConnectionBuilder

Builds Wi-Fi connection settings with security configuration.

```rust
use nmrs::builders::WifiConnectionBuilder;

let settings = WifiConnectionBuilder::new("MyNetwork")
    .wpa_psk("my_password")
    .band(nmrs::builders::WifiBand::A) // 5 GHz
    .ipv4_auto()
    .build();
```

### WifiBand / WifiMode

```rust
pub enum WifiBand { Bg, A } // 2.4 GHz, 5 GHz
pub enum WifiMode { Infrastructure, Adhoc, Ap }
```

## WireGuardBuilder

Builds WireGuard VPN connection settings with validation.

```rust
use nmrs::builders::WireGuardBuilder;
use nmrs::WireGuardPeer;

let peer = WireGuardPeer::new(
    "HIgo9xNzJMWLKAShlKl6/bUT1VI9Q0SDBXGtLXkPFXc=",
    "vpn.example.com:51820",
    vec!["0.0.0.0/0".into()],
);

let settings = WireGuardBuilder::new("MyVPN")
    .private_key("YBk6X3pP8KjKz7+HFWzVHNqL3qTZq8hX9VxFQJ4zVmM=")
    .address("10.0.0.2/24")
    .add_peer(peer)
    .dns(vec!["1.1.1.1".into()])
    .mtu(1420)
    .autoconnect(false)
    .build()?;
```

The `build()` method validates all fields and returns `Result<Settings, ConnectionError>`.

### Validation

| Check | Error |
|-------|-------|
| Private key format | `InvalidPrivateKey` |
| Address CIDR format | `InvalidAddress` |
| At least one peer | `InvalidPeers` |
| Peer public key format | `InvalidPublicKey` |
| Gateway host:port format | `InvalidGateway` |
| Peer allowed IPs non-empty | `InvalidPeers` |

## Builder Functions

Convenience functions that wrap the builders:

```rust
use nmrs::builders::{build_wifi_connection, build_ethernet_connection};
use nmrs::{WifiSecurity, ConnectionOptions};

// Wi-Fi
let wifi = build_wifi_connection("MyNetwork", &WifiSecurity::Open, &ConnectionOptions::default());

// Ethernet
let eth = build_ethernet_connection("eth0", &ConnectionOptions::default());
```

## When to Use Builders

Use the builders when you need:
- Custom IP configuration (static IP, DNS, routes)
- Specific Wi-Fi band or mode settings
- Custom connection sections (bridge, bond, VLAN)
- Fine-grained control over the settings dictionary

For standard connections, the `NetworkManager` API handles everything automatically.

## Submitting Builder Output

Builders produce a NetworkManager settings dictionary
(`HashMap<&str, HashMap<&str, zvariant::Value>>`). To activate that profile you
need the same system D-Bus connection nmrs already manages, plus compatible
`zbus` / `zvariant` types from [`nmrs::raw`](./raw.md).

### Wi-Fi hotspot (AP mode)

This is the workflow for cases such as [#260](https://github.com/freedesktop-rs/nmrs/issues/260) where the high-level `connect()` API does not expose every builder knob (for example `WifiMode::Ap`):

```rust
use nmrs::builders::{WifiConnectionBuilder, WifiMode};
use nmrs::raw::{zbus, zvariant};
use nmrs::{NetworkManager, Result};

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager",
    default_service = "org.freedesktop.NetworkManager",
    default_path = "/org/freedesktop/NetworkManager"
)]
trait Nm {
    fn add_and_activate_connection(
        &self,
        connection: std::collections::HashMap<
            &str,
            std::collections::HashMap<&str, zvariant::Value<'_>>,
        >,
        device: zvariant::OwnedObjectPath,
        specific_object: zvariant::OwnedObjectPath,
    ) -> zbus::Result<(zvariant::OwnedObjectPath, zvariant::OwnedObjectPath)>;
}

async fn start_hotspot(nm: &NetworkManager, interface: &str) -> Result<()> {
    let settings = WifiConnectionBuilder::new("Hotspot")
        .wpa_psk("password")
        .mode(WifiMode::Ap)
        .ipv4_shared()
        .ipv6_ignore()
        .build();

    let device = nm.get_device_by_interface(interface).await?;
    let proxy = NmProxy::new(nm.dbus_connection()).await?;
    proxy
        .add_and_activate_connection(settings, device, "/".into())
        .await?;

    Ok(())
}
```

Notes:

- Use `"/"` as `specific_object` for AP mode and other cases where there is no target access point.
- For client (infrastructure) mode, resolve an access-point object path first (nmrs does this internally in `connect()`).
- Map D-Bus errors to `ConnectionError::Dbus` (or handle them in your own error type).
- nmrs does not yet provide a high-level wrapper for `AddConnection` / `AddAndActivateConnection`; `dbus_connection()` is the supported escape hatch.

### Saving without activating

To persist a profile without bringing it up immediately, define a proxy method for `AddConnection` instead and pass the same `settings` map. nmrs uses that D-Bus call internally when saving VPN profiles.

## OpenVpnBuilder

Builds OpenVPN connection settings from an `OpenVpnConfig` or by importing a `.ovpn` file.

### From Configuration

```rust
use nmrs::{OpenVpnConfig, OpenVpnAuthType};

let config = OpenVpnConfig::new("CorpVPN", "vpn.example.com", 1194, false)
    .with_auth_type(OpenVpnAuthType::PasswordTls)
    .with_username("user")
    .with_password("secret")
    .with_ca_cert("/etc/openvpn/ca.crt")
    .with_client_cert("/etc/openvpn/client.crt")
    .with_client_key("/etc/openvpn/client.key");
```

### From .ovpn File

```rust
use nmrs::builders::OpenVpnBuilder;

let config = OpenVpnBuilder::from_ovpn_file("client.ovpn")?
    .username("user")
    .password("secret")
    .build()?;
```

Or use the high-level API directly:

```rust
let nm = NetworkManager::new().await?;
nm.import_ovpn("client.ovpn", Some("user"), Some("secret")).await?;
```

## Full API Reference

See [docs.rs/nmrs](https://docs.rs/nmrs) for complete builder documentation.

## See Also

- [Raw Module](./raw.md) – `zbus` / `zvariant` re-exports for advanced D-Bus work
- [NetworkManager API](./network-manager.md#advanced-d-bus-access) – `dbus_connection()`
- [D-Bus Architecture](../advanced/dbus.md) – how settings reach NetworkManager
