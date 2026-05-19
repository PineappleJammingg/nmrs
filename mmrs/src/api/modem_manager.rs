//! High-level ModemManager entry point.

use std::collections::HashMap;
use std::net::Ipv4Addr;

use futures::future::try_join_all;
use zbus::Connection;
use zvariant::{OwnedObjectPath, OwnedValue, Str, Value};

use crate::api::models::{
    AccessTechnology, Bearer, BearerConfig, BearerStats, ConnectionStatus, Ip4Config, Modem,
    ModemError, ModemState, Result, Sim,
};
use crate::api::modem_scope::ModemScope;
use crate::dbus::{MMBearerProxy, MMManagerProxy, MMModemProxy, MMModemSimpleProxy, MMSimProxy};

const MODEM_MANAGER_SERVICE: &str = "org.freedesktop.ModemManager1";
const MODEM_MANAGER_PATH: &str = "/org/freedesktop/ModemManager1";
const MODEM_INTERFACE: &str = "org.freedesktop.ModemManager1.Modem";

const MM_INCORRECT_PASSWORD: &str =
    "org.freedesktop.ModemManager1.Error.MobileEquipment.IncorrectPassword";
const MM_INCORRECT_PIN: &str = "org.freedesktop.ModemManager1.Error.MobileEquipment.IncorrectPin";
const MM_INCORRECT_PUK: &str = "org.freedesktop.ModemManager1.Error.MobileEquipment.IncorrectPuk";

/// Bearer IP configuration method constants (`MM_BEARER_IP_METHOD_*`).
const MM_BEARER_IP_METHOD_UNKNOWN: u32 = 0;
const MM_BEARER_IP_METHOD_PPP: u32 = 1;
const MM_BEARER_IP_METHOD_STATIC: u32 = 2;
const MM_BEARER_IP_METHOD_DHCP: u32 = 3;

/// High-level interface to ModemManager over D-Bus.
///
/// This is the main entry point for enumerating modems, managing simple
/// packet-data connections, querying signal state, and working with SIM PINs.
#[derive(Debug, Clone)]
pub struct ModemManager {
    conn: Connection,
}

impl ModemManager {
    /// Connects to the system D-Bus and creates a new [`ModemManager`].
    pub async fn new() -> Result<Self> {
        let conn = Connection::system().await?;
        Self::with_connection(conn).await
    }

    /// Creates a [`ModemManager`] from an existing D-Bus connection.
    ///
    /// Validates that ModemManager is reachable on the bus by reading its
    /// version property. Returns an error immediately if the service is not
    /// running.
    pub async fn with_connection(conn: Connection) -> Result<Self> {
        let proxy = MMManagerProxy::new(&conn).await?;
        let _ = proxy.version().await?;
        Ok(Self { conn })
    }

    /// Returns the underlying D-Bus connection.
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Lists all modems currently managed by ModemManager.
    pub async fn list_modems(&self) -> Result<Vec<Modem>> {
        let paths = enumerate_modem_paths(&self.conn).await?;
        let futures: Vec<_> = paths
            .iter()
            .map(|path| self.modem_info_for_path(path.as_str()))
            .collect();
        try_join_all(futures).await
    }

    /// Returns the modem whose equipment identifier matches the given IMEI.
    ///
    /// Only reads the `EquipmentIdentifier` property from each modem path
    /// and fetches the full snapshot once a match is found, avoiding
    /// unnecessary D-Bus round-trips on multi-modem systems.
    pub async fn modem_by_imei(&self, imei: &str) -> Result<Modem> {
        let paths = enumerate_modem_paths(&self.conn).await?;
        for path in &paths {
            let proxy = modem_proxy(&self.conn, path).await?;
            if proxy.equipment_identifier().await? == imei {
                return self.modem_info_for_path(path).await;
            }
        }
        Err(ModemError::ModemNotFound(format!("IMEI {imei}")))
    }

    /// Returns the modem with the lowest-sorted object path.
    ///
    /// On single-modem systems this is the only modem. On multi-modem
    /// systems this is the modem whose path sorts first numerically by
    /// trailing index.
    pub async fn primary_modem(&self) -> Result<Modem> {
        let path = self.primary_modem_path().await?;
        self.modem_info_for_path(&path).await
    }

    /// Creates a scope for operating on a specific modem object path.
    #[must_use]
    pub fn modem(&self, path: &str) -> ModemScope<'_> {
        ModemScope::new(self, path)
    }

    /// Enables the primary modem.
    pub async fn enable(&self) -> Result<()> {
        let path = self.primary_modem_path().await?;
        self.enable_for_path(path.as_str()).await
    }

    /// Disables the primary modem.
    pub async fn disable(&self) -> Result<()> {
        let path = self.primary_modem_path().await?;
        self.disable_for_path(path.as_str()).await
    }

    /// Connects the primary modem using only an APN.
    ///
    /// Uses `Modem.Simple.Connect`, which lets ModemManager handle the
    /// one-shot enable, registration, and bearer connection flow.
    pub async fn connect_simple(&self, apn: &str) -> Result<Bearer> {
        self.connect(&BearerConfig::new(apn)).await
    }

    /// Connects the primary modem using a full bearer configuration.
    pub async fn connect(&self, config: &BearerConfig) -> Result<Bearer> {
        let path = self.primary_modem_path().await?;
        self.connect_for_path(path.as_str(), config).await
    }

    /// Disconnects all bearers on the primary modem.
    pub async fn disconnect(&self) -> Result<()> {
        let path = self.primary_modem_path().await?;
        self.disconnect_for_path(path.as_str()).await
    }

    /// Returns the primary modem's current connection status.
    pub async fn status(&self) -> Result<ConnectionStatus> {
        let path = self.primary_modem_path().await?;
        self.status_for_path(path.as_str()).await
    }

    /// Returns the primary modem's active SIM, if one is reported.
    pub async fn sim(&self) -> Result<Option<Sim>> {
        let path = self.primary_modem_path().await?;
        self.sim_for_path(path.as_str()).await
    }

    /// Sends a PIN to unlock the primary modem's SIM.
    pub async fn unlock_pin(&self, pin: &str) -> Result<()> {
        let path = self.primary_modem_path().await?;
        self.unlock_pin_for_path(path.as_str(), pin).await
    }

    /// Sends a PUK and new PIN to unlock the primary modem's SIM.
    pub async fn unlock_puk(&self, puk: &str, new_pin: &str) -> Result<()> {
        let path = self.primary_modem_path().await?;
        self.unlock_puk_for_path(path.as_str(), puk, new_pin).await
    }

    /// Enables or disables SIM PIN checking on the primary modem.
    pub async fn set_pin_enabled(&self, pin: &str, enabled: bool) -> Result<()> {
        let path = self.primary_modem_path().await?;
        self.set_pin_enabled_for_path(path.as_str(), pin, enabled)
            .await
    }

    /// Changes the primary modem SIM's PIN.
    pub async fn change_pin(&self, old: &str, new: &str) -> Result<()> {
        let path = self.primary_modem_path().await?;
        self.change_pin_for_path(path.as_str(), old, new).await
    }

    /// Returns the primary modem's current signal quality percentage.
    pub async fn signal_quality(&self) -> Result<u32> {
        let path = self.primary_modem_path().await?;
        self.signal_quality_for_path(path.as_str()).await
    }

    /// Returns the primary modem's current access technology bitmask.
    pub async fn access_technology(&self) -> Result<AccessTechnology> {
        let path = self.primary_modem_path().await?;
        self.access_technology_for_path(path.as_str()).await
    }

    pub(crate) async fn modem_info_for_path(&self, path: &str) -> Result<Modem> {
        let modem_path = modem_object_path(path)?;
        let proxy = MMModemProxy::builder(&self.conn)
            .path(modem_path.clone())?
            .build()
            .await?;

        let (signal_quality, recent) = proxy.signal_quality().await?;
        let signal_quality = if recent { signal_quality } else { 0 };
        let sim_path = proxy.sim().await?;
        let bearer_paths = proxy
            .bearers()
            .await?
            .into_iter()
            .map(|path| path.to_string())
            .collect();

        Ok(Modem {
            path: modem_path.to_string(),
            state: ModemState::from_raw(proxy.state().await?),
            manufacturer: proxy.manufacturer().await?,
            model: proxy.model().await?,
            equipment_identifier: proxy.equipment_identifier().await?,
            access_technologies: AccessTechnology::from(proxy.access_technologies().await?),
            signal_quality,
            primary_sim_path: object_path_option(&sim_path),
            bearer_paths,
        })
    }

    pub(crate) async fn enable_for_path(&self, path: &str) -> Result<()> {
        let proxy = modem_proxy(&self.conn, path).await?;
        proxy.enable(true).await?;
        Ok(())
    }

    pub(crate) async fn disable_for_path(&self, path: &str) -> Result<()> {
        let proxy = modem_proxy(&self.conn, path).await?;
        proxy.enable(false).await?;
        Ok(())
    }

    pub(crate) async fn connect_for_path(
        &self,
        path: &str,
        config: &BearerConfig,
    ) -> Result<Bearer> {
        if config.apn.trim().is_empty() {
            return Err(ModemError::InvalidApn(config.apn.clone()));
        }

        let proxy = modem_simple_proxy(&self.conn, path).await?;
        let bearer_path = proxy
            .connect(bearer_properties(config))
            .await
            .map_err(|e| ModemError::BearerCreationFailed(format!("Simple.Connect failed: {e}")))?;

        bearer_snapshot(&self.conn, &bearer_path).await
    }

    pub(crate) async fn disconnect_for_path(&self, path: &str) -> Result<()> {
        let proxy = modem_simple_proxy(&self.conn, path).await?;
        let all_bearers = OwnedObjectPath::try_from("/").map_err(|e| {
            ModemError::BearerDisconnectFailed(format!("invalid all-bearers path: {e}"))
        })?;

        proxy.disconnect(all_bearers).await.map_err(|e| {
            ModemError::BearerDisconnectFailed(format!("Simple.Disconnect failed: {e}"))
        })
    }

    pub(crate) async fn status_for_path(&self, path: &str) -> Result<ConnectionStatus> {
        let simple = modem_simple_proxy(&self.conn, path).await?;
        let status = simple.get_status().await?;
        let modem = self.modem_info_for_path(path).await?;

        let state = take_i32(&status, "state")
            .map(ModemState::from_raw)
            .unwrap_or(modem.state);
        let access_technology = take_u32(&status, "access-technology")
            .or_else(|| take_u32(&status, "access-technologies"))
            .map(AccessTechnology::from)
            .unwrap_or(modem.access_technologies);
        let signal_quality = take_u32(&status, "signal-quality");

        Ok(ConnectionStatus {
            modem_path: modem.path,
            state,
            connected: state.is_connected(),
            access_technology,
            signal_quality,
            bearer_paths: modem.bearer_paths,
        })
    }

    pub(crate) async fn sim_for_path(&self, path: &str) -> Result<Option<Sim>> {
        let modem = modem_proxy(&self.conn, path).await?;
        let sim_path = modem.sim().await?;
        if object_path_option(&sim_path).is_none() {
            return Ok(None);
        }

        let proxy = MMSimProxy::builder(&self.conn)
            .path(sim_path.clone())?
            .build()
            .await?;

        Ok(Some(Sim {
            path: sim_path.to_string(),
            active: proxy.active().await?,
            iccid: proxy.sim_identifier().await?,
            imsi: proxy.imsi().await?,
            operator_name: proxy.operator_name().await?,
        }))
    }

    pub(crate) async fn unlock_pin_for_path(&self, path: &str, pin: &str) -> Result<()> {
        let sim = sim_proxy_for_modem(&self.conn, path).await?;
        sim.send_pin(pin).await.map_err(classify_pin_error)
    }

    pub(crate) async fn unlock_puk_for_path(
        &self,
        path: &str,
        puk: &str,
        new_pin: &str,
    ) -> Result<()> {
        let sim = sim_proxy_for_modem(&self.conn, path).await?;
        sim.send_puk(puk, new_pin).await.map_err(classify_pin_error)
    }

    pub(crate) async fn set_pin_enabled_for_path(
        &self,
        path: &str,
        pin: &str,
        enabled: bool,
    ) -> Result<()> {
        let sim = sim_proxy_for_modem(&self.conn, path).await?;
        sim.enable_pin(pin, enabled)
            .await
            .map_err(classify_pin_error)
    }

    pub(crate) async fn change_pin_for_path(&self, path: &str, old: &str, new: &str) -> Result<()> {
        let sim = sim_proxy_for_modem(&self.conn, path).await?;
        sim.change_pin(old, new).await.map_err(classify_pin_error)
    }

    pub(crate) async fn signal_quality_for_path(&self, path: &str) -> Result<u32> {
        let proxy = modem_proxy(&self.conn, path).await?;
        let (quality, _) = proxy.signal_quality().await?;
        Ok(quality)
    }

    pub(crate) async fn access_technology_for_path(&self, path: &str) -> Result<AccessTechnology> {
        let proxy = modem_proxy(&self.conn, path).await?;
        Ok(AccessTechnology::from(proxy.access_technologies().await?))
    }

    async fn primary_modem_path(&self) -> Result<String> {
        enumerate_modem_paths(&self.conn)
            .await?
            .into_iter()
            .next()
            .ok_or(ModemError::NoModems)
    }
}

async fn enumerate_modem_paths(conn: &Connection) -> Result<Vec<String>> {
    let manager = zbus::fdo::ObjectManagerProxy::builder(conn)
        .destination(MODEM_MANAGER_SERVICE)?
        .path(MODEM_MANAGER_PATH)?
        .build()
        .await?;

    let objects = manager.get_managed_objects().await?;
    let mut paths: Vec<String> = objects
        .into_iter()
        .filter(|(_, ifaces)| ifaces.contains_key(MODEM_INTERFACE))
        .map(|(path, _)| path.to_string())
        .collect();
    paths.sort_by(|a, b| numeric_path_cmp(a, b));
    Ok(paths)
}

/// Compare two D-Bus object paths by trailing numeric index so that
/// `.../Modem/2` sorts before `.../Modem/10`.
fn numeric_path_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let trailing_num =
        |s: &str| -> Option<u64> { s.rsplit('/').next().and_then(|seg| seg.parse().ok()) };
    match (trailing_num(a), trailing_num(b)) {
        (Some(na), Some(nb)) => na.cmp(&nb),
        _ => a.cmp(b),
    }
}

async fn modem_proxy<'a>(conn: &'a Connection, path: &str) -> Result<MMModemProxy<'a>> {
    Ok(MMModemProxy::builder(conn)
        .path(modem_object_path(path)?)?
        .build()
        .await?)
}

async fn modem_simple_proxy<'a>(
    conn: &'a Connection,
    path: &str,
) -> Result<MMModemSimpleProxy<'a>> {
    Ok(MMModemSimpleProxy::builder(conn)
        .path(modem_object_path(path)?)?
        .build()
        .await?)
}

async fn sim_proxy_for_modem<'a>(conn: &'a Connection, path: &str) -> Result<MMSimProxy<'a>> {
    let modem = modem_proxy(conn, path).await?;
    let sim_path = modem.sim().await?;
    if object_path_option(&sim_path).is_none() {
        return Err(ModemError::NoSim);
    }

    Ok(MMSimProxy::builder(conn).path(sim_path)?.build().await?)
}

async fn bearer_snapshot(conn: &Connection, path: &OwnedObjectPath) -> Result<Bearer> {
    let proxy = MMBearerProxy::builder(conn)
        .path(path.clone())?
        .build()
        .await?;
    let ip4 = proxy.ip4_config().await?;
    let stats = proxy.stats().await?;

    Ok(Bearer {
        path: path.to_string(),
        interface: proxy.interface().await?,
        connected: proxy.connected().await?,
        ip4_config: decode_ip4_config(&ip4),
        stats: decode_bearer_stats(&stats),
    })
}

fn bearer_properties(config: &BearerConfig) -> HashMap<&str, Value<'_>> {
    let mut properties = HashMap::new();
    properties.insert("apn", Value::from(config.apn.as_str()));
    properties.insert("ip-type", Value::from(config.ip_type.as_raw()));
    properties.insert("allow-roaming", Value::from(config.allow_roaming));

    if let Some(user) = &config.user {
        properties.insert("user", Value::from(user.as_str()));
    }
    if let Some(password) = &config.password {
        properties.insert("password", Value::from(password.as_str()));
    }

    properties
}

fn modem_object_path(path: &str) -> Result<OwnedObjectPath> {
    OwnedObjectPath::try_from(path).map_err(|e| ModemError::InvalidObjectPath {
        path: path.to_string(),
        reason: e.to_string(),
    })
}

fn object_path_option(path: &OwnedObjectPath) -> Option<String> {
    let path = path.to_string();
    if path == "/" { None } else { Some(path) }
}

fn decode_ip4_config(values: &HashMap<String, OwnedValue>) -> Option<Ip4Config> {
    if values.is_empty() {
        return None;
    }

    let method =
        take_str(values, "method").or_else(|| take_u32(values, "method").and_then(ip_method_name));

    let address = take_str(values, "address").and_then(|value| value.parse().ok());

    if method.is_none() && address.is_none() {
        return None;
    }

    Some(Ip4Config {
        method: method.unwrap_or_default(),
        address,
        prefix: take_u32(values, "prefix").unwrap_or_default(),
        gateway: take_str(values, "gateway").and_then(|value| value.parse().ok()),
        dns: take_ipv4_vec(values, "dns"),
        mtu: take_u32(values, "mtu"),
    })
}

fn ip_method_name(raw: u32) -> Option<String> {
    match raw {
        MM_BEARER_IP_METHOD_UNKNOWN => None,
        MM_BEARER_IP_METHOD_PPP => Some("ppp".to_string()),
        MM_BEARER_IP_METHOD_STATIC => Some("static".to_string()),
        MM_BEARER_IP_METHOD_DHCP => Some("dhcp".to_string()),
        _ => None,
    }
}

fn decode_bearer_stats(values: &HashMap<String, OwnedValue>) -> BearerStats {
    BearerStats {
        rx_bytes: take_u64(values, "rx-bytes").unwrap_or_default(),
        tx_bytes: take_u64(values, "tx-bytes").unwrap_or_default(),
        duration_seconds: take_u32(values, "duration").unwrap_or_default(),
        attempts: take_u32(values, "attempts").unwrap_or_default(),
        failed_attempts: take_u32(values, "failed-attempts").unwrap_or_default(),
        total_duration_seconds: take_u32(values, "total-duration").unwrap_or_default(),
        total_rx_bytes: take_u64(values, "total-rx-bytes").unwrap_or_default(),
        total_tx_bytes: take_u64(values, "total-tx-bytes").unwrap_or_default(),
    }
}

fn take_str(values: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    values.get(key).and_then(owned_to_str)
}

fn take_u32(values: &HashMap<String, OwnedValue>, key: &str) -> Option<u32> {
    values.get(key).and_then(owned_to_u32)
}

fn take_i32(values: &HashMap<String, OwnedValue>, key: &str) -> Option<i32> {
    values.get(key).and_then(owned_to_i32)
}

fn take_u64(values: &HashMap<String, OwnedValue>, key: &str) -> Option<u64> {
    values.get(key).and_then(owned_to_u64)
}

fn take_ipv4_vec(values: &HashMap<String, OwnedValue>, key: &str) -> Vec<Ipv4Addr> {
    let Some(value) = values.get(key) else {
        return Vec::new();
    };

    if let Ok(strings) = Vec::<String>::try_from(value.clone()) {
        return strings
            .into_iter()
            .filter_map(|value| value.parse().ok())
            .collect();
    }

    if let Ok(numbers) = Vec::<u32>::try_from(value.clone()) {
        return numbers.into_iter().map(Ipv4Addr::from).collect();
    }

    Vec::new()
}

fn owned_to_str(value: &OwnedValue) -> Option<String> {
    Str::try_from(value.clone())
        .ok()
        .map(|value| value.to_string())
        .or_else(|| String::try_from(value.clone()).ok())
}

fn owned_to_u32(value: &OwnedValue) -> Option<u32> {
    u32::try_from(value.clone()).ok().or_else(|| {
        i32::try_from(value.clone())
            .ok()
            .and_then(|value| value.try_into().ok())
    })
}

fn owned_to_i32(value: &OwnedValue) -> Option<i32> {
    i32::try_from(value.clone()).ok().or_else(|| {
        u32::try_from(value.clone())
            .ok()
            .and_then(|value| value.try_into().ok())
    })
}

fn owned_to_u64(value: &OwnedValue) -> Option<u64> {
    u64::try_from(value.clone())
        .ok()
        .or_else(|| u32::try_from(value.clone()).ok().map(u64::from))
}

/// Classify a zbus error from a SIM PIN/PUK operation into the
/// appropriate [`ModemError`] variant by inspecting the structured
/// D-Bus error name rather than the (locale-dependent) human-readable
/// message.
fn classify_pin_error(error: zbus::Error) -> ModemError {
    let name = dbus_error_name(&error);
    if name.as_deref() == Some(MM_INCORRECT_PIN) || name.as_deref() == Some(MM_INCORRECT_PASSWORD) {
        return ModemError::WrongPin;
    }
    if name.as_deref() == Some(MM_INCORRECT_PUK) {
        return ModemError::WrongPuk;
    }
    ModemError::Dbus(error)
}

fn dbus_error_name(error: &zbus::Error) -> Option<String> {
    match error {
        zbus::Error::MethodError(name, _, _) => Some(name.to_string()),
        zbus::Error::FDO(boxed) => {
            use zbus::fdo::Error as FdoError;
            match boxed.as_ref() {
                FdoError::ZBus(inner) => dbus_error_name(inner),
                _ => None,
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_map(pairs: &[(&str, OwnedValue)]) -> HashMap<String, OwnedValue> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect()
    }

    fn str_val(s: &str) -> OwnedValue {
        Value::from(s).try_into().unwrap()
    }

    fn u32_val(n: u32) -> OwnedValue {
        Value::from(n).try_into().unwrap()
    }

    fn i32_val(n: i32) -> OwnedValue {
        Value::from(n).try_into().unwrap()
    }

    fn u64_val(n: u64) -> OwnedValue {
        Value::from(n).try_into().unwrap()
    }

    // --- take_* helpers ---

    #[test]
    fn take_str_returns_string_value() {
        let map = make_map(&[("method", str_val("ppp"))]);
        assert_eq!(take_str(&map, "method"), Some("ppp".to_string()));
    }

    #[test]
    fn take_str_returns_none_for_missing_key() {
        let map: HashMap<String, OwnedValue> = HashMap::new();
        assert_eq!(take_str(&map, "method"), None);
    }

    #[test]
    fn take_u32_from_u32_value() {
        let map = make_map(&[("prefix", u32_val(24))]);
        assert_eq!(take_u32(&map, "prefix"), Some(24));
    }

    #[test]
    fn take_u32_from_i32_value() {
        let map = make_map(&[("prefix", i32_val(24))]);
        assert_eq!(take_u32(&map, "prefix"), Some(24));
    }

    #[test]
    fn take_u32_returns_none_for_missing() {
        let map: HashMap<String, OwnedValue> = HashMap::new();
        assert_eq!(take_u32(&map, "prefix"), None);
    }

    #[test]
    fn take_i32_from_i32_value() {
        let map = make_map(&[("state", i32_val(-1))]);
        assert_eq!(take_i32(&map, "state"), Some(-1));
    }

    #[test]
    fn take_u64_from_u64_value() {
        let map = make_map(&[("rx-bytes", u64_val(123456))]);
        assert_eq!(take_u64(&map, "rx-bytes"), Some(123456));
    }

    #[test]
    fn take_u64_from_u32_value() {
        let map = make_map(&[("rx-bytes", u32_val(42))]);
        assert_eq!(take_u64(&map, "rx-bytes"), Some(42));
    }

    // --- object_path_option ---

    #[test]
    fn object_path_option_root_is_none() {
        let path = OwnedObjectPath::try_from("/").unwrap();
        assert!(object_path_option(&path).is_none());
    }

    #[test]
    fn object_path_option_real_path_is_some() {
        let path = OwnedObjectPath::try_from("/org/freedesktop/ModemManager1/SIM/0").unwrap();
        assert_eq!(
            object_path_option(&path),
            Some("/org/freedesktop/ModemManager1/SIM/0".to_string())
        );
    }

    // --- modem_object_path ---

    #[test]
    fn modem_object_path_valid() {
        let path = modem_object_path("/org/freedesktop/ModemManager1/Modem/0");
        assert!(path.is_ok());
    }

    #[test]
    fn modem_object_path_invalid_returns_invalid_object_path() {
        let err = modem_object_path("not a path").unwrap_err();
        assert!(
            matches!(err, ModemError::InvalidObjectPath { .. }),
            "expected InvalidObjectPath, got {err:?}"
        );
    }

    // --- ip_method_name ---

    #[test]
    fn ip_method_name_known_values() {
        assert_eq!(ip_method_name(0), None);
        assert_eq!(ip_method_name(1), Some("ppp".to_string()));
        assert_eq!(ip_method_name(2), Some("static".to_string()));
        assert_eq!(ip_method_name(3), Some("dhcp".to_string()));
        assert_eq!(ip_method_name(99), None);
    }

    // --- decode_ip4_config ---

    #[test]
    fn decode_ip4_config_empty_map_returns_none() {
        let map = HashMap::new();
        assert!(decode_ip4_config(&map).is_none());
    }

    #[test]
    fn decode_ip4_config_no_method_or_address_returns_none() {
        let map = make_map(&[("mtu", u32_val(1500))]);
        assert!(decode_ip4_config(&map).is_none());
    }

    #[test]
    fn decode_ip4_config_unknown_numeric_method_no_address_returns_none() {
        let map = make_map(&[("method", u32_val(0))]);
        assert!(decode_ip4_config(&map).is_none());
    }

    #[test]
    fn decode_ip4_config_string_method() {
        let map = make_map(&[
            ("method", str_val("static")),
            ("address", str_val("10.0.0.1")),
            ("prefix", u32_val(24)),
        ]);
        let cfg = decode_ip4_config(&map).unwrap();
        assert_eq!(cfg.method, "static");
        assert_eq!(cfg.address, Some(Ipv4Addr::new(10, 0, 0, 1)));
        assert_eq!(cfg.prefix, 24);
    }

    #[test]
    fn decode_ip4_config_numeric_method() {
        let map = make_map(&[("method", u32_val(1))]);
        let cfg = decode_ip4_config(&map).unwrap();
        assert_eq!(cfg.method, "ppp");
    }

    #[test]
    fn decode_ip4_config_address_only() {
        let map = make_map(&[("address", str_val("192.168.1.1"))]);
        let cfg = decode_ip4_config(&map).unwrap();
        assert_eq!(cfg.address, Some(Ipv4Addr::new(192, 168, 1, 1)));
        assert!(cfg.method.is_empty());
    }

    // --- decode_bearer_stats ---

    #[test]
    fn decode_bearer_stats_empty_map_is_zeroed() {
        let stats = decode_bearer_stats(&HashMap::new());
        assert_eq!(stats.rx_bytes, 0);
        assert_eq!(stats.tx_bytes, 0);
        assert_eq!(stats.duration_seconds, 0);
    }

    #[test]
    fn decode_bearer_stats_populates_fields() {
        let map = make_map(&[
            ("rx-bytes", u64_val(1000)),
            ("tx-bytes", u64_val(2000)),
            ("duration", u32_val(60)),
            ("attempts", u32_val(3)),
            ("failed-attempts", u32_val(1)),
        ]);
        let stats = decode_bearer_stats(&map);
        assert_eq!(stats.rx_bytes, 1000);
        assert_eq!(stats.tx_bytes, 2000);
        assert_eq!(stats.duration_seconds, 60);
        assert_eq!(stats.attempts, 3);
        assert_eq!(stats.failed_attempts, 1);
    }

    // --- classify_pin_error ---

    fn make_method_error(name: &str) -> zbus::Error {
        use zbus::message::Message;
        let call = Message::method_call("/", "Foo")
            .unwrap()
            .build(&())
            .unwrap();
        let reply = Message::error(&call.header(), name)
            .unwrap()
            .build(&"error detail")
            .unwrap();
        reply.into()
    }

    #[test]
    fn classify_pin_error_incorrect_pin() {
        let error = make_method_error(MM_INCORRECT_PIN);
        assert!(matches!(classify_pin_error(error), ModemError::WrongPin));
    }

    #[test]
    fn classify_pin_error_incorrect_password() {
        let error = make_method_error(MM_INCORRECT_PASSWORD);
        assert!(matches!(classify_pin_error(error), ModemError::WrongPin));
    }

    #[test]
    fn classify_pin_error_incorrect_puk() {
        let error = make_method_error(MM_INCORRECT_PUK);
        assert!(matches!(classify_pin_error(error), ModemError::WrongPuk));
    }

    #[test]
    fn classify_pin_error_other_falls_through() {
        let error = zbus::Error::InvalidReply;
        assert!(matches!(classify_pin_error(error), ModemError::Dbus(_)));
    }

    // --- numeric_path_cmp ---

    #[test]
    fn numeric_sort_orders_correctly() {
        let mut paths = [
            "/org/freedesktop/ModemManager1/Modem/10".to_string(),
            "/org/freedesktop/ModemManager1/Modem/2".to_string(),
            "/org/freedesktop/ModemManager1/Modem/1".to_string(),
        ];
        paths.sort_by(|a, b| numeric_path_cmp(a, b));
        assert_eq!(paths[0], "/org/freedesktop/ModemManager1/Modem/1");
        assert_eq!(paths[1], "/org/freedesktop/ModemManager1/Modem/2");
        assert_eq!(paths[2], "/org/freedesktop/ModemManager1/Modem/10");
    }

    #[test]
    fn numeric_sort_falls_back_to_lexicographic() {
        let mut paths = ["b/xyz".to_string(), "a/abc".to_string()];
        paths.sort_by(|a, b| numeric_path_cmp(a, b));
        assert_eq!(paths[0], "a/abc");
        assert_eq!(paths[1], "b/xyz");
    }
}
