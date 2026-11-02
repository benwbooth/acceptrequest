//! A private certificate authority for running the server on a self-signed setup.
//! The CA certificate goes on each client machine (`caFile`) and on the parent's phone.

use anyhow::{Context, Result};
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose,
};
use std::path::Path;
use time::{Duration, OffsetDateTime};

/// iOS refuses server certificates valid for longer than this.
const SERVER_DAYS: i64 = 825;
const CA_DAYS: i64 = 20 * 365;

fn write_private(path: &Path, contents: &str) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("writing {}", path.display()))?;
    f.write_all(contents.as_bytes())?;
    Ok(())
}

/// SHA-256 fingerprint of the first certificate in a PEM file, as colon-separated hex.
pub fn pem_fingerprint(path: &Path) -> Result<String> {
    use rustls::pki_types::{CertificateDer, pem::PemObject};
    use sha2::{Digest, Sha256};
    let cert = CertificateDer::pem_file_iter(path)
        .with_context(|| format!("reading {}", path.display()))?
        .next()
        .context("no certificate in file")??;
    Ok(Sha256::digest(cert.as_ref()).iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":"))
}

/// Write `ca.pem`/`ca.key` (kept if they already exist, so clients and phones stay trusted)
/// and a fresh `cert.pem`/`key.pem` for `names` (DNS names or IP addresses) into `dir`.
pub fn generate(dir: &Path, names: &[String]) -> Result<()> {
    anyhow::ensure!(!names.is_empty(), "give at least one host name or IP address");
    std::fs::create_dir_all(dir)?;
    let (ca_cert_path, ca_key_path) = (dir.join("ca.pem"), dir.join("ca.key"));
    let now = OffsetDateTime::now_utc();

    let (ca, ca_key) = if ca_cert_path.exists() && ca_key_path.exists() {
        let key = KeyPair::from_pem(&std::fs::read_to_string(&ca_key_path)?)?;
        let params = CertificateParams::from_ca_cert_pem(&std::fs::read_to_string(&ca_cert_path)?)?;
        println!("Reusing the existing CA in {}", dir.display());
        // Same name and key as the saved CA, so certificates it signs chain to the saved ca.pem.
        (params.self_signed(&key)?, key)
    } else {
        let key = KeyPair::generate()?;
        let mut params = CertificateParams::default();
        params.distinguished_name.push(DnType::CommonName, "acceptrequest private CA");
        params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        params.not_before = now - Duration::days(1);
        params.not_after = now + Duration::days(CA_DAYS);
        let cert = params.self_signed(&key)?;
        std::fs::write(&ca_cert_path, cert.pem())?;
        write_private(&ca_key_path, &key.serialize_pem())?;
        println!("Created a new CA in {}", dir.display());
        (cert, key)
    };

    let key = KeyPair::generate()?;
    let mut params = CertificateParams::new(names.to_vec())?;
    params.distinguished_name.push(DnType::CommonName, names[0].clone());
    params.is_ca = IsCa::ExplicitNoCa;
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    params.not_before = now - Duration::days(1);
    params.not_after = now + Duration::days(SERVER_DAYS);
    let cert = params.signed_by(&key, &ca, &ca_key)?;
    std::fs::write(dir.join("cert.pem"), cert.pem())?;
    write_private(&dir.join("key.pem"), &key.serialize_pem())?;
    println!(
        "Wrote cert.pem and key.pem for {} (valid {SERVER_DAYS} days).\n\n\
         Computers can trust this server by its fingerprint, with nothing to install:\n  \
         services.acceptrequest.client.serverFingerprint = \"{}\";\n\
         (It changes whenever you run gen-cert again.)\n\n\
         For phone notifications the phone must trust the certificate: install ca.pem on it,\n\
         or use a certificate from Let's Encrypt or `tailscale cert` instead.",
        names.join(", "),
        pem_fingerprint(&dir.join("cert.pem"))?
    );
    Ok(())
}
