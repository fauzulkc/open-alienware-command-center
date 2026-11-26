//! Day and night: the keyboard brighter in daylight and dimmer after dark, following the real sun where you
//! are (`lighting.daylight`).
//!
//! - Where: Windows' time zone names a city ([`CITIES`]: one per zone, the zone's own or its main one), so
//!   nothing is looked up online and no location permission is needed. The region setting isn't used: it is
//!   often where the laptop was bought, not where it is (the owner's says Canada, the clock Dhaka). A city can
//!   be chosen, or a latitude and longitude typed in.
//! - When: the sun's height over the horizon, from the date and that place (NOAA's solar equations, good to a
//!   minute or so), so the season and the latitude are in it: long summer evenings far north, short winter days,
//!   twelve-hour days near the equator, the polar night.
//! - How bright: the night level while the sun is more than 6° below the horizon (after civil dusk), the day
//!   level once it is 6° above, and an eased blend through dawn and dusk, so the change is gradual.
//!
//! [`pct_at`] is the level the keyboard should show; the app looks every 30 s and passes it to the power rules
//! (`lightctl::PowerEvent::Daylight`), which send a brightness report when it moved.

use serde::{Deserialize, Serialize};

/// Below this sun height, degrees: night (civil dusk to civil dawn).
pub const NIGHT_ELEV: f64 = -6.0;
/// From this sun height, degrees: full day.
pub const DAY_ELEV: f64 = 6.0;
/// The sun's upper edge on the horizon, refraction included: sunrise and sunset.
pub const HORIZON: f64 = -0.833;

/// `lighting.daylight` in settings.json.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct DaylightSettings {
    pub on: bool,
    /// Brightness in daylight, %.
    pub day: u32,
    /// Brightness at night, %: 0 turns the keyboard off after dusk.
    pub night: u32,
    /// "auto" (from the time zone), a city from [`CITIES`], or "custom" ([`Self::lat`], [`Self::lon`]).
    pub place: String,
    /// Degrees, north positive (custom place).
    pub lat: f64,
    /// Degrees, east positive (custom place).
    pub lon: f64,
}

impl Default for DaylightSettings {
    fn default() -> Self {
        DaylightSettings { on: false, day: 100, night: 25, place: "auto".into(), lat: 0.0, lon: 0.0 }
    }
}

impl DaylightSettings {
    pub fn validate(&self) -> Result<(), String> {
        if self.day > 100 || self.night > 100 {
            return Err("day and night brightness are 0–100 %".into());
        }
        if !(self.lat.is_finite() && (-90.0..=90.0).contains(&self.lat)) {
            return Err(format!("latitude {} is out of range: -90 to 90", self.lat));
        }
        if !(self.lon.is_finite() && (-180.0..=180.0).contains(&self.lon)) {
            return Err(format!("longitude {} is out of range: -180 to 180", self.lon));
        }
        if self.place != "auto" && self.place != "custom" && city(&self.place).is_none() {
            return Err(format!("'{}' isn't a place: use auto, custom or a city from the list", self.place));
        }
        Ok(())
    }
}

/// A Windows time zone, the city it stands for, and where that is (degrees).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct City {
    pub zone: &'static str,
    pub name: &'static str,
    pub lat: f64,
    pub lon: f64,
}

const fn c(zone: &'static str, name: &'static str, lat: f64, lon: f64) -> City {
    City { zone, name, lat, lon }
}

/// Every Windows time zone with a place (the `UTC±n` zones and the old Mid-Atlantic have none), its city:
/// the one its name gives, or the biggest in it; for India its middle, Nagpur, as the zone spans the country.
pub const CITIES: &[City] = &[
    c("Aleutian Standard Time", "Adak", 51.88, -176.66),
    c("Hawaiian Standard Time", "Honolulu", 21.31, -157.86),
    c("Marquesas Standard Time", "Taiohae", -8.91, -140.10),
    c("Alaskan Standard Time", "Anchorage", 61.22, -149.90),
    c("British Columbia Standard Time", "Vancouver", 49.28, -123.12),
    c("Pacific Standard Time (Mexico)", "Tijuana", 32.51, -117.04),
    c("Pacific Standard Time", "Los Angeles", 34.05, -118.24),
    c("Alberta Standard Time", "Calgary", 51.05, -114.07),
    c("US Mountain Standard Time", "Phoenix", 33.45, -112.07),
    c("Mountain Standard Time (Mexico)", "Mazatlán", 23.25, -106.41),
    c("Mountain Standard Time", "Denver", 39.74, -104.99),
    c("Yukon Standard Time", "Whitehorse", 60.72, -135.06),
    c("Manitoba Standard Time", "Winnipeg", 49.90, -97.14),
    c("Central America Standard Time", "Guatemala City", 14.63, -90.51),
    c("Central Standard Time", "Chicago", 41.88, -87.63),
    c("Easter Island Standard Time", "Hanga Roa", -27.15, -109.43),
    c("Central Standard Time (Mexico)", "Mexico City", 19.43, -99.13),
    c("Canada Central Standard Time", "Regina", 50.45, -104.61),
    c("SA Pacific Standard Time", "Bogotá", 4.71, -74.07),
    c("Eastern Standard Time (Mexico)", "Chetumal", 18.50, -88.30),
    c("Eastern Standard Time", "New York", 40.71, -74.01),
    c("Haiti Standard Time", "Port-au-Prince", 18.54, -72.34),
    c("Cuba Standard Time", "Havana", 23.11, -82.37),
    c("US Eastern Standard Time", "Indianapolis", 39.77, -86.16),
    c("Turks And Caicos Standard Time", "Cockburn Town", 21.46, -71.14),
    c("Atlantic Standard Time", "Halifax", 44.65, -63.58),
    c("Venezuela Standard Time", "Caracas", 10.48, -66.90),
    c("Central Brazilian Standard Time", "Cuiabá", -15.60, -56.10),
    c("SA Western Standard Time", "Manaus", -3.12, -60.02),
    c("Pacific SA Standard Time", "Santiago", -33.45, -70.67),
    c("Newfoundland Standard Time", "St. John's", 47.56, -52.71),
    c("Tocantins Standard Time", "Araguaína", -7.19, -48.21),
    c("Paraguay Standard Time", "Asunción", -25.26, -57.58),
    c("E. South America Standard Time", "Brasília", -15.79, -47.88),
    c("SA Eastern Standard Time", "Fortaleza", -3.73, -38.52),
    c("Argentina Standard Time", "Buenos Aires", -34.60, -58.38),
    c("Montevideo Standard Time", "Montevideo", -34.90, -56.16),
    c("Magallanes Standard Time", "Punta Arenas", -53.16, -70.91),
    c("Saint Pierre Standard Time", "Saint-Pierre", 46.78, -56.18),
    c("Bahia Standard Time", "Salvador", -12.97, -38.50),
    c("Greenland Standard Time", "Nuuk", 64.18, -51.69),
    c("Azores Standard Time", "Ponta Delgada", 37.74, -25.67),
    c("Cape Verde Standard Time", "Praia", 14.93, -23.51),
    c("Morocco Standard Time", "Casablanca", 33.57, -7.59),
    c("GMT Standard Time", "London", 51.51, -0.13),
    c("Greenwich Standard Time", "Monrovia", 6.30, -10.80),
    c("Sao Tome Standard Time", "São Tomé", 0.34, 6.73),
    c("W. Europe Standard Time", "Berlin", 52.52, 13.40),
    c("Central Europe Standard Time", "Budapest", 47.50, 19.04),
    c("Romance Standard Time", "Paris", 48.86, 2.35),
    c("Central European Standard Time", "Warsaw", 52.23, 21.01),
    c("W. Central Africa Standard Time", "Lagos", 6.52, 3.38),
    c("GTB Standard Time", "Athens", 37.98, 23.73),
    c("Middle East Standard Time", "Beirut", 33.89, 35.50),
    c("Egypt Standard Time", "Cairo", 30.04, 31.24),
    c("E. Europe Standard Time", "Chișinău", 47.01, 28.86),
    c("West Bank Standard Time", "Hebron", 31.53, 35.10),
    c("South Africa Standard Time", "Pretoria", -25.75, 28.19),
    c("FLE Standard Time", "Kyiv", 50.45, 30.52),
    c("Israel Standard Time", "Jerusalem", 31.77, 35.21),
    c("South Sudan Standard Time", "Juba", 4.85, 31.58),
    c("Kaliningrad Standard Time", "Kaliningrad", 54.71, 20.51),
    c("Sudan Standard Time", "Khartoum", 15.50, 32.56),
    c("Libya Standard Time", "Tripoli", 32.89, 13.19),
    c("Namibia Standard Time", "Windhoek", -22.56, 17.08),
    c("Jordan Standard Time", "Amman", 31.95, 35.93),
    c("Arabic Standard Time", "Baghdad", 33.31, 44.36),
    c("Syria Standard Time", "Damascus", 33.51, 36.28),
    c("Turkey Standard Time", "Istanbul", 41.01, 28.98),
    c("Arab Standard Time", "Riyadh", 24.71, 46.68),
    c("Belarus Standard Time", "Minsk", 53.90, 27.56),
    c("Russian Standard Time", "Moscow", 55.76, 37.62),
    c("E. Africa Standard Time", "Nairobi", -1.29, 36.82),
    c("Volgograd Standard Time", "Volgograd", 48.71, 44.51),
    c("Iran Standard Time", "Tehran", 35.69, 51.39),
    c("Arabian Standard Time", "Dubai", 25.20, 55.27),
    c("Astrakhan Standard Time", "Astrakhan", 46.35, 48.04),
    c("Azerbaijan Standard Time", "Baku", 40.41, 49.87),
    c("Russia Time Zone 3", "Samara", 53.20, 50.15),
    c("Mauritius Standard Time", "Port Louis", -20.16, 57.50),
    c("Saratov Standard Time", "Saratov", 51.53, 46.03),
    c("Georgian Standard Time", "Tbilisi", 41.72, 44.79),
    c("Caucasus Standard Time", "Yerevan", 40.18, 44.51),
    c("Afghanistan Standard Time", "Kabul", 34.56, 69.21),
    c("West Asia Standard Time", "Tashkent", 41.30, 69.24),
    c("Qyzylorda Standard Time", "Astana", 51.17, 71.45),
    c("Ekaterinburg Standard Time", "Yekaterinburg", 56.84, 60.61),
    c("Pakistan Standard Time", "Karachi", 24.86, 67.01),
    c("India Standard Time", "Nagpur (central India)", 21.15, 79.09),
    c("Sri Lanka Standard Time", "Colombo", 6.93, 79.86),
    c("Nepal Standard Time", "Kathmandu", 27.72, 85.32),
    c("Central Asia Standard Time", "Bishkek", 42.87, 74.59),
    c("Bangladesh Standard Time", "Dhaka", 23.81, 90.41),
    c("Omsk Standard Time", "Omsk", 54.99, 73.37),
    c("Myanmar Standard Time", "Yangon", 16.87, 96.20),
    c("SE Asia Standard Time", "Bangkok", 13.76, 100.50),
    c("Altai Standard Time", "Barnaul", 53.35, 83.78),
    c("W. Mongolia Standard Time", "Khovd", 48.01, 91.64),
    c("North Asia Standard Time", "Krasnoyarsk", 56.01, 92.89),
    c("N. Central Asia Standard Time", "Novosibirsk", 55.03, 82.92),
    c("Tomsk Standard Time", "Tomsk", 56.48, 84.95),
    c("China Standard Time", "Beijing", 39.90, 116.41),
    c("North Asia East Standard Time", "Irkutsk", 52.29, 104.28),
    c("Singapore Standard Time", "Singapore", 1.35, 103.82),
    c("W. Australia Standard Time", "Perth", -31.95, 115.86),
    c("Taipei Standard Time", "Taipei", 25.03, 121.57),
    c("Ulaanbaatar Standard Time", "Ulaanbaatar", 47.89, 106.91),
    c("Aus Central W. Standard Time", "Eucla", -31.68, 128.88),
    c("Transbaikal Standard Time", "Chita", 52.03, 113.50),
    c("Tokyo Standard Time", "Tokyo", 35.68, 139.69),
    c("North Korea Standard Time", "Pyongyang", 39.04, 125.76),
    c("Korea Standard Time", "Seoul", 37.57, 126.98),
    c("Yakutsk Standard Time", "Yakutsk", 62.03, 129.73),
    c("Cen. Australia Standard Time", "Adelaide", -34.93, 138.60),
    c("AUS Central Standard Time", "Darwin", -12.46, 130.84),
    c("E. Australia Standard Time", "Brisbane", -27.47, 153.03),
    c("AUS Eastern Standard Time", "Sydney", -33.87, 151.21),
    c("West Pacific Standard Time", "Port Moresby", -9.44, 147.18),
    c("Tasmania Standard Time", "Hobart", -42.88, 147.33),
    c("Vladivostok Standard Time", "Vladivostok", 43.12, 131.89),
    c("Lord Howe Standard Time", "Lord Howe Island", -31.55, 159.08),
    c("Bougainville Standard Time", "Arawa", -6.23, 155.57),
    c("Russia Time Zone 10", "Chokurdakh", 70.62, 147.90),
    c("Magadan Standard Time", "Magadan", 59.56, 150.80),
    c("Norfolk Standard Time", "Kingston (Norfolk Island)", -29.06, 167.96),
    c("Sakhalin Standard Time", "Yuzhno-Sakhalinsk", 46.96, 142.73),
    c("Central Pacific Standard Time", "Honiara", -9.43, 159.95),
    c("Russia Time Zone 11", "Petropavlovsk-Kamchatsky", 53.04, 158.65),
    c("New Zealand Standard Time", "Auckland", -36.85, 174.76),
    c("Fiji Standard Time", "Suva", -18.14, 178.44),
    c("Kamchatka Standard Time", "Petropavlovsk-Kamchatsky (old zone)", 53.04, 158.65),
    c("Chatham Islands Standard Time", "Waitangi (Chatham Islands)", -43.95, -176.56),
    c("Tonga Standard Time", "Nuku'alofa", -21.14, -175.20),
    c("Samoa Standard Time", "Apia", -13.83, -171.76),
    c("Line Islands Standard Time", "Kiritimati", 1.87, -157.43),
];

/// A city by its name in [`CITIES`].
pub fn city(name: &str) -> Option<&'static City> {
    CITIES.iter().find(|c| c.name == name)
}

/// The city for a Windows time zone id.
pub fn city_of_zone(zone: &str) -> Option<&'static City> {
    CITIES.iter().find(|c| c.zone == zone)
}

/// Where the sun is reckoned for.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Place {
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    /// "zone" (the time zone's city), "city" (chosen), "custom" (typed in), "estimate" (no city for the zone:
    /// the longitude from the clock's offset, the equator for latitude).
    pub from: &'static str,
    /// Windows' time zone id, when it was read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zone: Option<String>,
}

/// The place for these settings: chosen or typed in, else the time zone's city (`zone`), else an estimate from
/// the clock's offset (`offset_s`, local minus UTC).
pub fn place_for(d: &DaylightSettings, zone: Option<&str>, offset_s: i64) -> Place {
    let zone_s = zone.map(str::to_string);
    if d.place == "custom" {
        return Place { name: format!("{:.2}°, {:.2}°", d.lat, d.lon), lat: d.lat, lon: d.lon, from: "custom", zone: zone_s };
    }
    if let Some(c) = city(&d.place) {
        return Place { name: c.name.into(), lat: c.lat, lon: c.lon, from: "city", zone: zone_s };
    }
    if let Some(c) = zone.and_then(city_of_zone) {
        return Place { name: c.name.into(), lat: c.lat, lon: c.lon, from: "zone", zone: zone_s };
    }
    let lon = ((offset_s as f64 / 3_600.0) * 15.0).clamp(-180.0, 180.0);
    Place { name: format!("UTC{:+}", offset_s as f64 / 3_600.0), lat: 0.0, lon, from: "estimate", zone: zone_s }
}

/// The place for these settings on this machine.
pub fn place(d: &DaylightSettings) -> Place {
    place_for(d, zone_id().as_deref(), local_offset_s())
}

// ---------------------------------------------------------------------------------------------------------
// The sun (NOAA's solar position equations).

/// The sun's height above the horizon at `unix_ms`, degrees (no refraction).
pub fn elevation(unix_ms: i64, lat: f64, lon: f64) -> f64 {
    let jd = unix_ms as f64 / 86_400_000.0 + 2_440_587.5;
    let t = (jd - 2_451_545.0) / 36_525.0;
    let l0 = (280.466_46 + t * (36_000.769_83 + t * 0.000_303_2)).rem_euclid(360.0);
    let m = 357.529_11 + t * (35_999.050_29 - 0.000_153_7 * t);
    let e = 0.016_708_634 - t * (0.000_042_037 + 0.000_000_126_7 * t);
    let mr = m.to_radians();
    let c = mr.sin() * (1.914_602 - t * (0.004_817 + 0.000_014 * t)) + (2.0 * mr).sin() * (0.019_993 - 0.000_101 * t) + (3.0 * mr).sin() * 0.000_289;
    let omega = (125.04 - 1_934.136 * t).to_radians();
    let lambda = (l0 + c - 0.005_69 - 0.004_78 * omega.sin()).to_radians();
    let eps0 = 23.0 + (26.0 + (21.448 - t * (46.815 + t * (0.000_59 - t * 0.001_813))) / 60.0) / 60.0;
    let eps = (eps0 + 0.002_56 * omega.cos()).to_radians();
    let decl = (eps.sin() * lambda.sin()).asin();
    let y = (eps / 2.0).tan().powi(2);
    let l0r = l0.to_radians();
    let eot = 4.0 * (y * (2.0 * l0r).sin() - 2.0 * e * mr.sin() + 4.0 * e * y * mr.sin() * (2.0 * l0r).cos() - 0.5 * y * y * (4.0 * l0r).sin() - 1.25 * e * e * (2.0 * mr).sin()).to_degrees();
    let utc_min = (unix_ms.rem_euclid(86_400_000)) as f64 / 60_000.0;
    let solar_min = (utc_min + eot + 4.0 * lon).rem_euclid(1_440.0);
    let ha = (solar_min / 4.0 - 180.0).to_radians();
    let latr = lat.to_radians();
    let cos_z = (latr.sin() * decl.sin() + latr.cos() * decl.cos() * ha.cos()).clamp(-1.0, 1.0);
    90.0 - cos_z.acos().to_degrees()
}

/// How much of the day level shows at sun height `elev`: 0 at night, 1 in daylight, eased in between.
pub fn share(elev: f64) -> f64 {
    let x = ((elev - NIGHT_ELEV) / (DAY_ELEV - NIGHT_ELEV)).clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// The brightness for sun height `elev`, %.
pub fn pct_for(d: &DaylightSettings, elev: f64) -> u32 {
    let (n, day) = (d.night.min(100) as f64, d.day.min(100) as f64);
    (n + (day - n) * share(elev)).round() as u32
}

/// The brightness the keyboard should show at `unix_ms` at `place`, %.
pub fn pct_at(d: &DaylightSettings, p: &Place, unix_ms: i64) -> u32 {
    pct_for(d, elevation(unix_ms, p.lat, p.lon))
}

/// The level now when day and night is on, for the power rules (`PowerState::daylight`).
pub fn now_pct(d: &DaylightSettings) -> Option<u32> {
    d.on.then(|| pct_at(d, &place(d), now_ms()))
}

// ---------------------------------------------------------------------------------------------------------
// Today, for the Lighting page.

/// Today at the place: the sun's times (ms since the epoch; `None` when it doesn't happen, as in the polar
/// night) and the brightness through the day.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Today {
    pub place: Place,
    pub now: i64,
    pub now_pct: u32,
    /// The sun's height now, degrees.
    pub elevation: f64,
    /// "day", "night", "dawn" or "dusk".
    pub phase: &'static str,
    /// Local midnight that starts today, and the clock's offset from UTC, s.
    pub day_start: i64,
    pub offset_s: i64,
    pub dawn: Option<i64>,
    pub sunrise: Option<i64>,
    pub noon: i64,
    pub sunset: Option<i64>,
    pub dusk: Option<i64>,
    /// The sun's highest today, degrees.
    pub noon_elevation: f64,
    /// Sunrise to sunset, minutes (0 in the polar night, 1440 under the midnight sun).
    pub day_min: u32,
    /// Every 15 minutes from local midnight: [ms, brightness %, the sun's height in degrees].
    pub curve: Vec<(i64, u32, f64)>,
}

/// When the sun passes `level` going up (`rising`) or down during the 24 h after `start`, to the second.
fn crossing(start: i64, lat: f64, lon: f64, level: f64, rising: bool) -> Option<i64> {
    let step = 60_000;
    let mut prev = elevation(start, lat, lon);
    for i in 1..=1_440 {
        let t = start + i * step;
        let e = elevation(t, lat, lon);
        let hit = if rising { prev < level && e >= level } else { prev >= level && e < level };
        if hit {
            let k = (level - prev) / (e - prev);
            return Some(t - step + (k * step as f64).round() as i64);
        }
        prev = e;
    }
    None
}

pub fn today_at(d: &DaylightSettings, place: Place, now: i64, offset_s: i64) -> Today {
    let off = offset_s * 1_000;
    let local = now + off;
    let day_start = local - local.rem_euclid(86_400_000) - off;
    let (lat, lon) = (place.lat, place.lon);
    let elev = elevation(now, lat, lon);
    let (noon, noon_elevation) = (0..=1_440).map(|m| day_start + m * 60_000).map(|t| (t, elevation(t, lat, lon))).fold((day_start, f64::MIN), |a, b| if b.1 > a.1 { b } else { a });
    let sunrise = crossing(day_start, lat, lon, HORIZON, true);
    let sunset = crossing(day_start, lat, lon, HORIZON, false);
    let day_min = match (sunrise, sunset) {
        (Some(r), Some(s)) if s > r => ((s - r) / 60_000) as u32,
        (Some(r), Some(s)) => 1_440 - ((r - s) / 60_000) as u32,
        _ if noon_elevation > HORIZON => 1_440,
        _ => 0,
    };
    let phase = if elev >= DAY_ELEV {
        "day"
    } else if elev <= NIGHT_ELEV {
        "night"
    } else if elevation(now + 300_000, lat, lon) > elev {
        "dawn"
    } else {
        "dusk"
    };
    Today {
        now,
        now_pct: pct_for(d, elev),
        elevation: elev,
        phase,
        day_start,
        offset_s,
        dawn: crossing(day_start, lat, lon, NIGHT_ELEV, true),
        sunrise,
        noon,
        sunset,
        dusk: crossing(day_start, lat, lon, NIGHT_ELEV, false),
        noon_elevation,
        day_min,
        curve: (0..=96)
            .map(|i| day_start + i * 900_000)
            .map(|t| {
                let e = elevation(t, lat, lon);
                (t, pct_for(d, e), (e * 10.0).round() / 10.0)
            })
            .collect(),
        place,
    }
}

/// Today on this machine.
pub fn today(d: &DaylightSettings) -> Today {
    today_at(d, place(d), now_ms(), local_offset_s())
}

// ---------------------------------------------------------------------------------------------------------
// The clock.

pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Days from 1970-01-01 to a civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The local clock's offset from UTC now, seconds (local minus UTC), rounded to the quarter hour.
pub fn local_offset_s() -> i64 {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::SYSTEMTIME;
        use windows::Win32::System::SystemInformation::{GetLocalTime, GetSystemTime};
        let secs = |t: SYSTEMTIME| days_from_civil(t.wYear as i64, t.wMonth as i64, t.wDay as i64) * 86_400 + t.wHour as i64 * 3_600 + t.wMinute as i64 * 60 + t.wSecond as i64;
        let (local, utc) = unsafe { (GetLocalTime(), GetSystemTime()) };
        let raw = secs(local) - secs(utc);
        (raw as f64 / 900.0).round() as i64 * 900
    }
    #[cfg(not(windows))]
    {
        let _ = days_from_civil;
        0
    }
}

/// Windows' time zone id ("Bangladesh Standard Time"), the key the zone table uses.
pub fn zone_id() -> Option<String> {
    #[cfg(windows)]
    {
        use windows::Win32::System::Time::{GetDynamicTimeZoneInformation, DYNAMIC_TIME_ZONE_INFORMATION};
        let mut tz = DYNAMIC_TIME_ZONE_INFORMATION::default();
        unsafe { GetDynamicTimeZoneInformation(&mut tz) };
        let n = tz.TimeZoneKeyName.iter().position(|&c| c == 0).unwrap_or(tz.TimeZoneKeyName.len());
        let s = String::from_utf16_lossy(&tz.TimeZoneKeyName[..n]);
        (!s.is_empty()).then_some(s)
    }
    #[cfg(not(windows))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ms since the epoch for a UTC date and time.
    fn utc(y: i64, mo: i64, d: i64, h: i64, mi: i64) -> i64 {
        (days_from_civil(y, mo, d) * 86_400 + h * 3_600 + mi * 60) * 1_000
    }

    fn at(name: &str) -> Place {
        let c = city(name).unwrap();
        Place { name: c.name.into(), lat: c.lat, lon: c.lon, from: "city", zone: None }
    }

    /// "hh:mm" local for a time, with the offset in hours.
    fn hm(t: Option<i64>, off_h: f64) -> String {
        let t = t.expect("happens today");
        let m = ((t as f64 / 60_000.0 + off_h * 60.0).round() as i64).rem_euclid(1_440);
        format!("{:02}:{:02}", m / 60, m % 60)
    }

    fn close(t: Option<i64>, off_h: f64, want: &str) {
        let got = hm(t, off_h);
        let mins = |s: &str| s[..2].parse::<i64>().unwrap() * 60 + s[3..].parse::<i64>().unwrap();
        assert!((mins(&got) - mins(want)).abs() <= 3, "got {got}, published {want}");
    }

    #[test]
    fn sun_times_match_published_tables() {
        let d = DaylightSettings::default();
        // London, midsummer and midwinter 2024 (BST +1 / GMT): sunrise 04:43 / 08:04, sunset 21:21 / 15:54.
        let june = today_at(&d, at("London"), utc(2024, 6, 21, 12, 0), 3_600);
        close(june.sunrise, 1.0, "04:43");
        close(june.sunset, 1.0, "21:21");
        let dec = today_at(&d, at("London"), utc(2024, 12, 21, 12, 0), 0);
        close(dec.sunrise, 0.0, "08:04");
        close(dec.sunset, 0.0, "15:54");
        assert!(june.day_min > dec.day_min + 480, "the season shows: {} vs {} min", june.day_min, dec.day_min);
        // Dhaka, 26 November 2026 (+6): sunrise about 06:19, sunset about 17:11.
        let dhaka = today_at(&d, at("Dhaka"), utc(2026, 11, 26, 6, 0), 6 * 3_600);
        close(dhaka.sunrise, 6.0, "06:19");
        close(dhaka.sunset, 6.0, "17:11");
        // Civil dawn and dusk sit about 23 minutes outside sunrise and sunset in the tropics.
        let gap = (dhaka.sunrise.unwrap() - dhaka.dawn.unwrap()) / 60_000;
        assert!((18..=28).contains(&gap), "dawn to sunrise: {gap} min");
        assert!(dhaka.noon > dhaka.sunrise.unwrap() && dhaka.noon < dhaka.sunset.unwrap());
    }

    #[test]
    fn the_polar_night_and_the_midnight_sun() {
        let d = DaylightSettings::default();
        let tromso = Place { name: "Tromsø".into(), lat: 69.65, lon: 18.96, from: "custom", zone: None };
        let winter = today_at(&d, tromso.clone(), utc(2024, 12, 21, 11, 0), 3_600);
        assert_eq!((winter.sunrise, winter.sunset, winter.day_min), (None, None, 0), "polar night: no sunrise");
        assert!(winter.dawn.is_some(), "but a civil twilight at noon");
        let summer = today_at(&d, tromso, utc(2024, 6, 21, 11, 0), 7_200);
        assert_eq!((summer.sunrise, summer.sunset, summer.day_min), (None, None, 1_440), "midnight sun");
        // At midnight the sun dips to about 3° over the horizon there: not quite full day, never night.
        assert!(summer.curve.iter().all(|(_, p, _)| *p >= 85), "bright all day: {:?}", summer.curve.iter().map(|c| c.1).min());
    }

    #[test]
    fn brightness_eases_from_night_to_day() {
        let d = DaylightSettings { on: true, day: 100, night: 20, ..Default::default() };
        assert_eq!((pct_for(&d, -20.0), pct_for(&d, -6.0), pct_for(&d, 0.0), pct_for(&d, 6.0), pct_for(&d, 60.0)), (20, 20, 60, 100, 100));
        let mut last = 0;
        for i in -80..=80 {
            let p = pct_for(&d, i as f64 / 10.0);
            assert!(p >= last, "never goes back down on the way up");
            last = p;
        }
        // Through a real dawn the level climbs over about the hour around sunrise (the tropics' quick
        // twilight; longer further from the equator), never in a jump.
        let dhaka = today_at(&d, at("Dhaka"), utc(2026, 11, 26, 6, 0), 6 * 3_600);
        let steps: Vec<i64> = dhaka.curve.windows(2).map(|w| (w[1].1 as i64 - w[0].1 as i64).abs()).collect();
        assert!(steps.iter().all(|s| *s <= 35), "15-minute steps stay small: {steps:?}");
        assert!(steps.iter().filter(|s| **s > 0).count() >= 8, "dawn and dusk each take an hour or so");
        let london = today_at(&d, at("London"), utc(2024, 12, 21, 12, 0), 0);
        let slow = london.curve.windows(2).filter(|w| w[0].1 != w[1].1).count();
        assert!(slow > steps.iter().filter(|s| **s > 0).count(), "a winter dawn in London is slower than one in Dhaka");
        assert_eq!(dhaka.curve.len(), 97);
        assert_eq!((dhaka.curve[0].1, dhaka.curve[48].1), (20, 100), "night at midnight, day at noon");
        // Night above day is allowed (an inverted schedule).
        let inv = DaylightSettings { day: 10, night: 90, ..d };
        assert_eq!((pct_for(&inv, -30.0), pct_for(&inv, 30.0)), (90, 10));
    }

    #[test]
    fn the_place_comes_from_the_time_zone_unless_chosen() {
        let d = DaylightSettings::default();
        let p = place_for(&d, Some("Bangladesh Standard Time"), 6 * 3_600);
        assert_eq!((p.name.as_str(), p.from), ("Dhaka", "zone"));
        let chosen = place_for(&DaylightSettings { place: "Toronto".into(), ..d.clone() }, Some("Eastern Standard Time"), 0);
        assert_eq!(chosen.from, "zone", "an unknown name falls back to the zone (validation refuses it on save)");
        let chosen = place_for(&DaylightSettings { place: "Tokyo".into(), ..d.clone() }, Some("Bangladesh Standard Time"), 0);
        assert_eq!((chosen.name.as_str(), chosen.from), ("Tokyo", "city"));
        let custom = place_for(&DaylightSettings { place: "custom".into(), lat: 43.65, lon: -79.38, ..d.clone() }, None, 0);
        assert_eq!((custom.lat, custom.lon, custom.from), (43.65, -79.38, "custom"));
        let est = place_for(&d, Some("UTC-11"), -11 * 3_600);
        assert_eq!((est.lon, est.from), (-165.0, "estimate"));
    }

    #[test]
    fn the_zone_table_is_sound() {
        let mut names: Vec<&str> = CITIES.iter().map(|c| c.name).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), CITIES.len(), "city names are unique (they are the setting's value)");
        let mut zones: Vec<&str> = CITIES.iter().map(|c| c.zone).collect();
        zones.sort();
        zones.dedup();
        assert_eq!(zones.len(), CITIES.len(), "one city per zone");
        assert!(CITIES.iter().all(|c| c.lat.abs() <= 90.0 && c.lon.abs() <= 180.0 && c.name != "auto" && c.name != "custom"));
        // Every zone this Windows knows (when the test runs on Windows) has a city or is a UTC offset.
        if let Some(z) = zone_id() {
            assert!(city_of_zone(&z).is_some() || z.starts_with("UTC") || z == "Dateline Standard Time" || z == "Mid-Atlantic Standard Time", "no city for {z}");
        }
    }

    #[test]
    fn settings_are_checked() {
        let ok = DaylightSettings::default();
        ok.validate().unwrap();
        let bad = |f: &dyn Fn(&mut DaylightSettings)| {
            let mut d = DaylightSettings::default();
            f(&mut d);
            d.validate().is_err()
        };
        assert!(bad(&|d| d.day = 101));
        assert!(bad(&|d| d.night = 150));
        assert!(bad(&|d| d.lat = 91.0));
        assert!(bad(&|d| d.lon = f64::NAN));
        assert!(bad(&|d| d.place = "Atlantis".into()));
        assert!(!bad(&|d| d.place = "Dhaka".into()));
        assert!(!bad(&|d| d.night = 0), "0 turns the keyboard off at night");
        let old: crate::lightctl::LightingSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(old.daylight, DaylightSettings::default(), "older settings files: off");
    }

    #[test]
    fn the_clock_offset_is_whole_quarter_hours() {
        let o = local_offset_s();
        assert_eq!(o % 900, 0);
        assert!(o.abs() <= 14 * 3_600);
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
    }
}
