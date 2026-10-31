/// A byte count for people: "812 bytes", "1.2 MB", "24.1 GB" (decimal units).
pub fn size(bytes: u64) -> String {
    if bytes < 1000 {
        return if bytes == 1 { "1 byte".into() } else { format!("{bytes} bytes") };
    }
    let mut value = bytes as f64;
    for unit in ["kB", "MB", "GB", "TB"] {
        value /= 1000.0;
        if value < 999.95 || unit == "TB" {
            return if value < 99.95 { format!("{value:.1} {unit}") } else { format!("{value:.0} {unit}") };
        }
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    #[test]
    fn formats_sizes() {
        assert_eq!(super::size(1), "1 byte");
        assert_eq!(super::size(812), "812 bytes");
        assert_eq!(super::size(1_200_000), "1.2 MB");
        assert_eq!(super::size(24_100_000), "24.1 MB");
        assert_eq!(super::size(512_000_000), "512 MB");
        assert_eq!(super::size(999_960), "1.0 MB");
    }
}
