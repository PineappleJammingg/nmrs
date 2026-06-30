//! Active connection enumeration and classification.

use std::collections::HashMap;

use zbus::Connection;
use zvariant::{OwnedObjectPath, OwnedValue, Str};

use crate::Result;
use crate::api::models::{
    ActiveConnection, ActiveConnectionState, ActiveOtherConnection, ActiveVpnConnection,
    ActiveWifiConnection, ActiveWiredConnection,
};
use crate::dbus::{
    NMAccessPointProxy, NMActiveConnectionProxy, NMDeviceProxy, NMProxy, NMSettingsConnectionProxy,
    NMWiredProxy, NMWirelessProxy,
};
use crate::types::constants::device_type;
use crate::util::utils::{decode_ssid_or_hidden, get_ip_addresses_from_active_connection};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActiveConnectionKind {
    Wired,
    Wifi,
    Vpn,
    Other,
}

struct ActiveConnectionBase {
    id: String,
    uuid: String,
    state: ActiveConnectionState,
    devices: Vec<OwnedObjectPath>,
    specific_object: OwnedObjectPath,
    connection_type: Option<String>,
    ip4_address: Option<String>,
    ip6_address: Option<String>,
}

/// Lists active NetworkManager connections classified into public model types.
pub(crate) async fn list_active_connections(conn: &Connection) -> Result<Vec<ActiveConnection>> {
    let nm = NMProxy::new(conn).await?;
    let active_paths = nm.active_connections().await?;

    let mut active_connections = Vec::new();
    for active_path in active_paths {
        let active = NMActiveConnectionProxy::builder(conn)
            .path(active_path.clone())?
            .build()
            .await?;
        let base = active_connection_base(conn, &active_path, &active).await?;
        active_connections.push(classify_active_connection(conn, base).await?);
    }

    Ok(active_connections)
}

async fn active_connection_base(
    conn: &Connection,
    active_path: &OwnedObjectPath,
    active: &NMActiveConnectionProxy<'_>,
) -> Result<ActiveConnectionBase> {
    let id = active.id().await?;
    let uuid = active.uuid().await?;
    let state = ActiveConnectionState::from(active.state().await?);
    let devices = active.devices().await?;
    let specific_object = active.specific_object().await?;
    let connection_type = match active.connection().await {
        Ok(path) if path.as_str() != "/" => connection_type_for_settings_path(conn, path).await,
        _ => None,
    };
    let (ip4_address, ip6_address) =
        get_ip_addresses_from_active_connection(conn, active_path).await;

    Ok(ActiveConnectionBase {
        id,
        uuid,
        state,
        devices,
        specific_object,
        connection_type,
        ip4_address,
        ip6_address,
    })
}

async fn classify_active_connection(
    conn: &Connection,
    base: ActiveConnectionBase,
) -> Result<ActiveConnection> {
    let primary_device = base.devices.first().cloned();
    let device_summary = match primary_device {
        Some(path) => device_summary(conn, path).await?,
        None => DeviceSummary::default(),
    };

    match active_connection_kind(device_summary.device_type, base.connection_type.as_deref()) {
        ActiveConnectionKind::Wired => Ok(ActiveConnection::Wired(
            wired_connection(conn, base, device_summary).await,
        )),
        ActiveConnectionKind::Wifi => Ok(ActiveConnection::Wifi(
            wifi_connection(conn, base, device_summary).await,
        )),
        ActiveConnectionKind::Vpn => {
            Ok(ActiveConnection::Vpn(vpn_connection(base, device_summary)))
        }
        ActiveConnectionKind::Other => Ok(ActiveConnection::Other(other_connection(
            base,
            device_summary,
        ))),
    }
}

#[derive(Default)]
struct DeviceSummary {
    path: Option<OwnedObjectPath>,
    device_type: Option<u32>,
    interface: Option<String>,
    hw_address: Option<String>,
}

async fn device_summary(conn: &Connection, path: OwnedObjectPath) -> Result<DeviceSummary> {
    let device = NMDeviceProxy::builder(conn)
        .path(path.clone())?
        .build()
        .await?;

    Ok(DeviceSummary {
        path: Some(path),
        device_type: device.device_type().await.ok(),
        interface: device.interface().await.ok(),
        hw_address: device.hw_address().await.ok(),
    })
}

async fn wired_connection(
    conn: &Connection,
    base: ActiveConnectionBase,
    device: DeviceSummary,
) -> ActiveWiredConnection {
    let speed_mbps = match device.path {
        Some(path) => async {
            let wired = NMWiredProxy::builder(conn).path(path)?.build().await?;
            wired.speed().await
        }
        .await
        .ok(),
        None => None,
    };

    ActiveWiredConnection {
        id: base.id,
        uuid: base.uuid,
        interface: device.interface,
        hw_address: device.hw_address,
        speed_mbps,
        ip4_address: base.ip4_address,
        ip6_address: base.ip6_address,
        state: base.state,
    }
}

async fn wifi_connection(
    conn: &Connection,
    base: ActiveConnectionBase,
    device: DeviceSummary,
) -> ActiveWifiConnection {
    let ap_path = active_access_point_path(conn, device.path.as_ref(), &base.specific_object).await;
    let ap = match ap_path {
        Some(path) => active_access_point_summary(conn, path).await,
        None => None,
    };

    ActiveWifiConnection {
        id: base.id.clone(),
        uuid: base.uuid,
        ssid: ap
            .as_ref()
            .and_then(|ap| ap.ssid.clone())
            .unwrap_or(base.id),
        interface: device.interface,
        bssid: ap.as_ref().and_then(|ap| ap.bssid.clone()),
        strength: ap.and_then(|ap| ap.strength),
        ip4_address: base.ip4_address,
        ip6_address: base.ip6_address,
        state: base.state,
    }
}

fn vpn_connection(base: ActiveConnectionBase, device: DeviceSummary) -> ActiveVpnConnection {
    ActiveVpnConnection {
        id: base.id,
        uuid: base.uuid,
        interface: device.interface,
        ip4_address: base.ip4_address,
        ip6_address: base.ip6_address,
        state: base.state,
    }
}

fn other_connection(base: ActiveConnectionBase, device: DeviceSummary) -> ActiveOtherConnection {
    ActiveOtherConnection {
        id: base.id,
        uuid: base.uuid,
        connection_type: base.connection_type,
        interface: device.interface,
        ip4_address: base.ip4_address,
        ip6_address: base.ip6_address,
        state: base.state,
    }
}

async fn active_access_point_path(
    conn: &Connection,
    device_path: Option<&OwnedObjectPath>,
    specific_object: &OwnedObjectPath,
) -> Option<OwnedObjectPath> {
    if specific_object.as_str() != "/" {
        return Some(specific_object.clone());
    }

    let device_path = device_path?;
    let wifi = NMWirelessProxy::builder(conn)
        .path(device_path.clone())
        .ok()?
        .build()
        .await
        .ok()?;
    wifi.active_access_point()
        .await
        .ok()
        .filter(|path| path.as_str() != "/")
}

struct AccessPointSummary {
    ssid: Option<String>,
    bssid: Option<String>,
    strength: Option<u8>,
}

async fn active_access_point_summary(
    conn: &Connection,
    path: OwnedObjectPath,
) -> Option<AccessPointSummary> {
    let ap = NMAccessPointProxy::builder(conn)
        .path(path)
        .ok()?
        .build()
        .await
        .ok()?;

    Some(AccessPointSummary {
        ssid: ap
            .ssid()
            .await
            .ok()
            .map(|bytes| decode_ssid_or_hidden(&bytes).into_owned()),
        bssid: ap.hw_address().await.ok(),
        strength: ap.strength().await.ok(),
    })
}

async fn connection_type_for_settings_path(
    conn: &Connection,
    path: OwnedObjectPath,
) -> Option<String> {
    let settings = NMSettingsConnectionProxy::builder(conn)
        .path(path)
        .ok()?
        .build()
        .await
        .ok()?
        .get_settings()
        .await
        .ok()?;

    connection_type_from_settings(&settings)
}

fn connection_type_from_settings(
    settings: &HashMap<String, HashMap<String, OwnedValue>>,
) -> Option<String> {
    settings
        .get("connection")?
        .get("type")
        .and_then(owned_to_string)
}

fn owned_to_string(value: &OwnedValue) -> Option<String> {
    Str::try_from(value.clone())
        .ok()
        .map(|value| value.to_string())
        .or_else(|| String::try_from(value.clone()).ok())
}

fn active_connection_kind(
    raw_device_type: Option<u32>,
    connection_type: Option<&str>,
) -> ActiveConnectionKind {
    match raw_device_type {
        Some(device_type::ETHERNET) => ActiveConnectionKind::Wired,
        Some(device_type::WIFI) => ActiveConnectionKind::Wifi,
        _ if matches!(connection_type, Some("vpn" | "wireguard")) => ActiveConnectionKind::Vpn,
        _ => ActiveConnectionKind::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn classifies_by_device_type_first() {
        assert_eq!(
            active_connection_kind(Some(device_type::ETHERNET), Some("vpn")),
            ActiveConnectionKind::Wired
        );
        assert_eq!(
            active_connection_kind(Some(device_type::WIFI), Some("vpn")),
            ActiveConnectionKind::Wifi
        );
    }

    #[test]
    fn classifies_vpn_without_device_type() {
        assert_eq!(
            active_connection_kind(None, Some("vpn")),
            ActiveConnectionKind::Vpn
        );
        assert_eq!(
            active_connection_kind(None, Some("wireguard")),
            ActiveConnectionKind::Vpn
        );
    }

    #[test]
    fn classifies_unknown_as_other() {
        assert_eq!(
            active_connection_kind(Some(999), Some("bridge")),
            ActiveConnectionKind::Other
        );
        assert_eq!(
            active_connection_kind(None, None),
            ActiveConnectionKind::Other
        );
    }

    #[test]
    fn extracts_connection_type_from_settings() {
        let mut connection = HashMap::new();
        connection.insert("type".to_string(), OwnedValue::from(Str::from("vpn")));
        let mut settings = HashMap::new();
        settings.insert("connection".to_string(), connection);

        assert_eq!(
            connection_type_from_settings(&settings).as_deref(),
            Some("vpn")
        );
    }
}
