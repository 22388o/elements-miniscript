use std::{error, fmt, str::FromStr};

use bitcoin::{
    self,
    hashes::{hash160, hex::FromHex},
    secp256k1,
    secp256k1::{Secp256k1, Signing},
    util::bip32,
};

use MiniscriptKey;
use NullCtx;
use ToPublicKey;

/// The MiniscriptKey corresponding to Descriptors. This can
/// either be Single public key or a Xpub
#[derive(Debug, Eq, PartialEq, Clone, Ord, PartialOrd, Hash)]
pub enum DescriptorPublicKey {
    /// Single Public Key
    SinglePub(DescriptorSinglePub),
    /// Xpub
    XPub(DescriptorXKey<bip32::ExtendedPubKey>),
}

/// A Single Descriptor Key with optional origin information
#[derive(Debug, Eq, PartialEq, Clone, Ord, PartialOrd, Hash)]
pub struct DescriptorSinglePub {
    /// Origin information
    pub origin: Option<(bip32::Fingerprint, bip32::DerivationPath)>,
    /// The key
    pub key: bitcoin::PublicKey,
}

/// A Single Descriptor Secret Key with optional origin information
#[derive(Debug)]
pub struct DescriptorSinglePriv {
    /// Origin information
    pub origin: Option<bip32::KeySource>,
    /// The key
    pub key: bitcoin::PrivateKey,
}

/// A Secret Key that can be either a single key or an Xprv
#[derive(Debug)]
pub enum DescriptorSecretKey {
    /// Single Secret Key
    SinglePriv(DescriptorSinglePriv),
    /// Xprv
    XPrv(DescriptorXKey<bip32::ExtendedPrivKey>),
}

impl fmt::Display for DescriptorSecretKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            &DescriptorSecretKey::SinglePriv(ref sk) => {
                maybe_fmt_master_id(f, &sk.origin)?;
                sk.key.fmt(f)?;
                Ok(())
            }
            &DescriptorSecretKey::XPrv(ref xprv) => {
                maybe_fmt_master_id(f, &xprv.origin)?;
                xprv.xkey.fmt(f)?;
                fmt_derivation_path(f, &xprv.derivation_path)?;
                if xprv.is_wildcard {
                    write!(f, "/*")?;
                }
                Ok(())
            }
        }
    }
}

/// Trait for "extended key" types like `xpub` and `xprv`. Used internally to generalize parsing and
/// handling of `bip32::ExtendedPubKey` and `bip32::ExtendedPrivKey`.
pub trait InnerXKey: fmt::Display + FromStr {
    /// Returns the fingerprint of the key
    fn xkey_fingerprint<C: Signing>(&self, secp: &Secp256k1<C>) -> bip32::Fingerprint;

    /// Returns whether hardened steps can be derived on the key
    ///
    /// `true` for `bip32::ExtendedPrivKey` and `false` for `bip32::ExtendedPubKey`.
    fn can_derive_hardened() -> bool;
}

impl InnerXKey for bip32::ExtendedPubKey {
    fn xkey_fingerprint<C: Signing>(&self, _secp: &Secp256k1<C>) -> bip32::Fingerprint {
        self.fingerprint()
    }

    fn can_derive_hardened() -> bool {
        false
    }
}

impl InnerXKey for bip32::ExtendedPrivKey {
    fn xkey_fingerprint<C: Signing>(&self, secp: &Secp256k1<C>) -> bip32::Fingerprint {
        self.fingerprint(secp)
    }

    fn can_derive_hardened() -> bool {
        true
    }
}

/// Instance of an extended key with origin and derivation path
#[derive(Debug, Eq, PartialEq, Clone, Ord, PartialOrd, Hash)]
pub struct DescriptorXKey<K: InnerXKey> {
    /// Origin information
    pub origin: Option<(bip32::Fingerprint, bip32::DerivationPath)>,
    /// The extended key
    pub xkey: K,
    /// The derivation path
    pub derivation_path: bip32::DerivationPath,
    /// Whether the descriptor is wildcard
    pub is_wildcard: bool,
}

impl DescriptorSinglePriv {
    /// Returns the public key of this key
    fn as_public<C: Signing>(
        &self,
        secp: &Secp256k1<C>,
    ) -> Result<DescriptorSinglePub, DescriptorKeyParseError> {
        let pub_key = self.key.public_key(secp);

        Ok(DescriptorSinglePub {
            origin: self.origin.clone(),
            key: pub_key,
        })
    }
}

impl DescriptorXKey<bip32::ExtendedPrivKey> {
    /// Returns the public version of this key, applying all the hardened derivation steps on the
    /// private key before turning it into a public key.
    ///
    /// If the key already has an origin, the derivation steps applied will be appended to the path
    /// already present, otherwise this key will be treated as "root" key and an origin will be
    /// added with this key's fingerprint and the derivation steps applied.
    fn as_public<C: Signing>(
        &self,
        secp: &Secp256k1<C>,
    ) -> Result<DescriptorXKey<bip32::ExtendedPubKey>, DescriptorKeyParseError> {
        let path_len = (&self.derivation_path).as_ref().len();
        let public_suffix_len = (&self.derivation_path)
            .into_iter()
            .rev()
            .take_while(|c| c.is_normal())
            .count();

        let derivation_path = &self.derivation_path[(path_len - public_suffix_len)..];
        let deriv_on_hardened = &self.derivation_path[..(path_len - public_suffix_len)];

        let derived_xprv = self
            .xkey
            .derive_priv(&secp, &deriv_on_hardened)
            .map_err(|_| DescriptorKeyParseError("Unable to derive the hardened steps"))?;
        let xpub = bip32::ExtendedPubKey::from_private(&secp, &derived_xprv);

        let origin = match &self.origin {
            &Some((fingerprint, ref origin_path)) => Some((
                fingerprint,
                origin_path
                    .into_iter()
                    .chain(deriv_on_hardened.into_iter())
                    .cloned()
                    .collect(),
            )),
            &None if !deriv_on_hardened.as_ref().is_empty() => {
                Some((self.xkey.fingerprint(&secp), deriv_on_hardened.into()))
            }
            _ => self.origin.clone(),
        };

        Ok(DescriptorXKey {
            origin,
            xkey: xpub,
            derivation_path: derivation_path.into(),
            is_wildcard: self.is_wildcard,
        })
    }
}

/// Descriptor Key parsing errors
// FIXME: replace with error enums
#[derive(Debug, PartialEq, Clone, Copy)]
pub struct DescriptorKeyParseError(&'static str);

impl fmt::Display for DescriptorKeyParseError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl error::Error for DescriptorKeyParseError {}

impl fmt::Display for DescriptorPublicKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            DescriptorPublicKey::SinglePub(ref pk) => {
                maybe_fmt_master_id(f, &pk.origin)?;
                pk.key.fmt(f)?;
                Ok(())
            }
            DescriptorPublicKey::XPub(ref xpub) => {
                maybe_fmt_master_id(f, &xpub.origin)?;
                xpub.xkey.fmt(f)?;
                fmt_derivation_path(f, &xpub.derivation_path)?;
                if xpub.is_wildcard {
                    write!(f, "/*")?;
                }
                Ok(())
            }
        }
    }
}

impl DescriptorSecretKey {
    /// Return the public version of this key, by applying either
    /// [`DescriptorSinglePriv::as_public`] or [`DescriptorXKey<bip32::ExtendedPrivKey>::as_public`]
    /// depending on the type of key.
    ///
    /// If the key is an "XPrv", the hardened derivation steps will be applied before converting it
    /// to a public key. See the documentation of [`DescriptorXKey<bip32::ExtendedPrivKey>::as_public`]
    /// for more details.
    pub fn as_public<C: Signing>(
        &self,
        secp: &Secp256k1<C>,
    ) -> Result<DescriptorPublicKey, DescriptorKeyParseError> {
        Ok(match self {
            &DescriptorSecretKey::SinglePriv(ref sk) => {
                DescriptorPublicKey::SinglePub(sk.as_public(secp)?)
            }
            &DescriptorSecretKey::XPrv(ref xprv) => {
                DescriptorPublicKey::XPub(xprv.as_public(secp)?)
            }
        })
    }
}

/// Writes the fingerprint of the origin, if there is one.
fn maybe_fmt_master_id(
    f: &mut fmt::Formatter,
    origin: &Option<(bip32::Fingerprint, bip32::DerivationPath)>,
) -> fmt::Result {
    if let Some((ref master_id, ref master_deriv)) = *origin {
        fmt::Formatter::write_str(f, "[")?;
        for byte in master_id.into_bytes().iter() {
            write!(f, "{:02x}", byte)?;
        }
        fmt_derivation_path(f, master_deriv)?;
        fmt::Formatter::write_str(f, "]")?;
    }

    Ok(())
}

/// Writes a derivation path to the formatter, no leading 'm'
fn fmt_derivation_path(f: &mut fmt::Formatter, path: &bip32::DerivationPath) -> fmt::Result {
    for child in path {
        write!(f, "/{}", child)?;
    }
    Ok(())
}

impl FromStr for DescriptorPublicKey {
    type Err = DescriptorKeyParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // A "raw" public key without any origin is the least we accept.
        if s.len() < 66 {
            return Err(DescriptorKeyParseError(
                "Key too short (<66 char), doesn't match any format",
            ));
        }

        let (key_part, origin) = DescriptorXKey::<bip32::ExtendedPubKey>::parse_xkey_origin(s)?;

        if key_part.contains("pub") {
            let (xpub, derivation_path, is_wildcard) =
                DescriptorXKey::<bip32::ExtendedPubKey>::parse_xkey_deriv(key_part)?;

            Ok(DescriptorPublicKey::XPub(DescriptorXKey {
                origin,
                xkey: xpub,
                derivation_path,
                is_wildcard,
            }))
        } else {
            if key_part.len() >= 2
                && !(&key_part[0..2] == "02" || &key_part[0..2] == "03" || &key_part[0..2] == "04")
            {
                return Err(DescriptorKeyParseError(
                    "Only publickeys with prefixes 02/03/04 are allowed",
                ));
            }
            let key = bitcoin::PublicKey::from_str(key_part)
                .map_err(|_| DescriptorKeyParseError("Error while parsing simple public key"))?;
            Ok(DescriptorPublicKey::SinglePub(DescriptorSinglePub {
                key,
                origin,
            }))
        }
    }
}

impl DescriptorPublicKey {
    /// Derives the specified child key if self is a wildcard xpub. Otherwise returns self.
    ///
    /// Panics if given a hardened child number
    pub fn derive(self, child_number: bip32::ChildNumber) -> DescriptorPublicKey {
        debug_assert!(child_number.is_normal());

        match self {
            DescriptorPublicKey::SinglePub(_) => self,
            DescriptorPublicKey::XPub(xpub) => {
                if xpub.is_wildcard {
                    DescriptorPublicKey::XPub(DescriptorXKey {
                        origin: xpub.origin,
                        xkey: xpub.xkey,
                        derivation_path: xpub.derivation_path.into_child(child_number),
                        is_wildcard: false,
                    })
                } else {
                    DescriptorPublicKey::XPub(xpub)
                }
            }
        }
    }
}

impl FromStr for DescriptorSecretKey {
    type Err = DescriptorKeyParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (key_part, origin) = DescriptorXKey::<bip32::ExtendedPubKey>::parse_xkey_origin(s)?;

        if key_part.len() <= 52 {
            let sk = bitcoin::PrivateKey::from_str(key_part)
                .map_err(|_| DescriptorKeyParseError("Error while parsing a WIF private key"))?;
            Ok(DescriptorSecretKey::SinglePriv(DescriptorSinglePriv {
                key: sk,
                origin: None,
            }))
        } else {
            let (xprv, derivation_path, is_wildcard) =
                DescriptorXKey::<bip32::ExtendedPrivKey>::parse_xkey_deriv(key_part)?;
            Ok(DescriptorSecretKey::XPrv(DescriptorXKey {
                origin,
                xkey: xprv,
                derivation_path,
                is_wildcard,
            }))
        }
    }
}

impl<K: InnerXKey> DescriptorXKey<K> {
    fn parse_xkey_origin(
        s: &str,
    ) -> Result<(&str, Option<(bip32::Fingerprint, bip32::DerivationPath)>), DescriptorKeyParseError>
    {
        for ch in s.as_bytes() {
            if *ch < 20 || *ch > 127 {
                return Err(DescriptorKeyParseError(
                    "Encountered an unprintable character",
                ));
            }
        }

        if s.is_empty() {
            return Err(DescriptorKeyParseError("Empty key"));
        }
        let mut parts = s[1..].split(']');

        if let Some('[') = s.chars().next() {
            let mut raw_origin = parts
                .next()
                .ok_or(DescriptorKeyParseError("Unclosed '['"))?
                .split('/');

            let origin_id_hex = raw_origin.next().ok_or(DescriptorKeyParseError(
                "No master fingerprint found after '['",
            ))?;

            if origin_id_hex.len() != 8 {
                return Err(DescriptorKeyParseError(
                    "Master fingerprint should be 8 characters long",
                ));
            }
            let parent_fingerprint = bip32::Fingerprint::from_hex(origin_id_hex).map_err(|_| {
                DescriptorKeyParseError("Malformed master fingerprint, expected 8 hex chars")
            })?;
            let origin_path = raw_origin
                .map(|p| bip32::ChildNumber::from_str(p))
                .collect::<Result<bip32::DerivationPath, bip32::Error>>()
                .map_err(|_| {
                    DescriptorKeyParseError("Error while parsing master derivation path")
                })?;

            let key = parts
                .next()
                .ok_or(DescriptorKeyParseError("No key after origin."))?;

            if parts.next().is_some() {
                Err(DescriptorKeyParseError(
                    "Multiple ']' in Descriptor Public Key",
                ))
            } else {
                Ok((key, Some((parent_fingerprint, origin_path))))
            }
        } else {
            Ok((s, None))
        }
    }

    /// Parse an extended key concatenated to a derivation path.
    fn parse_xkey_deriv(
        key_deriv: &str,
    ) -> Result<(K, bip32::DerivationPath, bool), DescriptorKeyParseError> {
        let mut key_deriv = key_deriv.split('/');
        let xkey_str = key_deriv.next().ok_or(DescriptorKeyParseError(
            "No key found after origin description",
        ))?;
        let xkey = K::from_str(xkey_str)
            .map_err(|_| DescriptorKeyParseError("Error while parsing xkey."))?;

        let mut is_wildcard = false;
        let derivation_path = key_deriv
            .filter_map(|p| {
                if !is_wildcard && p == "*" {
                    is_wildcard = true;
                    None
                } else if !is_wildcard && p == "*'" {
                    Some(Err(DescriptorKeyParseError(
                        "Hardened derivation is currently not supported.",
                    )))
                } else if is_wildcard {
                    Some(Err(DescriptorKeyParseError(
                        "'*' may only appear as last element in a derivation path.",
                    )))
                } else {
                    Some(bip32::ChildNumber::from_str(p).map_err(|_| {
                        DescriptorKeyParseError("Error while parsing key derivation path")
                    }))
                }
            })
            .collect::<Result<bip32::DerivationPath, _>>()?;

        if !K::can_derive_hardened() && !(&derivation_path).into_iter().all(|c| c.is_normal()) {
            Err(DescriptorKeyParseError(
                "Hardened derivation is currently not supported.",
            ))
        } else {
            Ok((xkey, derivation_path, is_wildcard))
        }
    }

    /// Compares this key with a `keysource` and returns the matching derivation path, if any.
    ///
    /// For keys that have an origin, the `keysource`'s fingerprint will be compared
    /// with the origin's fingerprint, and the `keysource`'s path will be compared with the concatenation of the
    /// origin's and key's paths.
    ///
    /// If the key `is_wildcard`, the last item of the `keysource`'s path will be ignored,
    ///
    /// ## Examples
    ///
    /// ```
    /// # extern crate elements_miniscript as miniscript;
    /// # use std::str::FromStr;
    /// # fn body() -> Result<(), Box<dyn std::error::Error>> {
    /// use miniscript::bitcoin::util::bip32;
    /// use miniscript::descriptor::DescriptorPublicKey;
    ///
    /// let ctx = miniscript::bitcoin::secp256k1::Secp256k1::signing_only();
    ///
    /// let key = DescriptorPublicKey::from_str("[d34db33f/44'/0'/0']xpub6ERApfZwUNrhLCkDtcHTcxd75RbzS1ed54G1LkBUHQVHQKqhMkhgbmJbZRkrgZw4koxb5JaHWkY4ALHY2grBGRjaDMzQLcgJvLJuZZvRcEL/1/*")?;
    /// let xpub = match key {
    ///     DescriptorPublicKey::XPub(xpub) => xpub,
    ///     _ => panic!("Parsing Error"),
    /// };
    ///
    /// assert_eq!(xpub.matches(&(bip32::Fingerprint::from_str("d34db33f")?, bip32::DerivationPath::from_str("m/44'/0'/0'/1/42")?), &ctx), Some(bip32::DerivationPath::from_str("m/44'/0'/0'/1")?));
    /// assert_eq!(xpub.matches(&(bip32::Fingerprint::from_str("ffffffff")?, bip32::DerivationPath::from_str("m/44'/0'/0'/1/42")?), &ctx), None);
    /// assert_eq!(xpub.matches(&(bip32::Fingerprint::from_str("d34db33f")?, bip32::DerivationPath::from_str("m/44'/0'/0'/100/0")?), &ctx), None);
    /// # Ok(())
    /// # }
    /// # body().unwrap()
    /// ```
    pub fn matches<C: Signing>(
        &self,
        keysource: &bip32::KeySource,
        secp: &Secp256k1<C>,
    ) -> Option<bip32::DerivationPath> {
        let (fingerprint, path) = keysource;

        let (compare_fingerprint, compare_path) = match &self.origin {
            &Some((fingerprint, ref path)) => (
                fingerprint,
                path.into_iter()
                    .chain(self.derivation_path.into_iter())
                    .collect(),
            ),
            &None => (
                self.xkey.xkey_fingerprint(secp),
                self.derivation_path.into_iter().collect::<Vec<_>>(),
            ),
        };

        let path_excluding_wildcard = if self.is_wildcard && path.as_ref().len() > 0 {
            path.into_iter()
                .take(path.as_ref().len() - 1)
                .cloned()
                .collect()
        } else {
            path.clone()
        };

        if &compare_fingerprint == fingerprint
            && compare_path
                .into_iter()
                .eq(path_excluding_wildcard.into_iter())
        {
            Some(path_excluding_wildcard)
        } else {
            None
        }
    }
}

impl MiniscriptKey for DescriptorPublicKey {
    // This allows us to be able to derive public keys even for PkH s
    type Hash = Self;

    fn to_pubkeyhash(&self) -> Self {
        self.clone()
    }
}

/// Context information for deriving a public key from DescriptorPublicKey
#[derive(Debug)]
pub struct DescriptorPublicKeyCtx<'secp, C: 'secp + secp256k1::Verification> {
    /// The underlying secp context
    secp_ctx: &'secp secp256k1::Secp256k1<C>,
    /// The child_number in case the descriptor is wildcard
    /// If the DescriptorPublicKey is not wildcard this field is not used.
    child_number: bip32::ChildNumber,
}

impl<'secp, C: secp256k1::Verification> Clone for DescriptorPublicKeyCtx<'secp, C> {
    fn clone(&self) -> Self {
        Self {
            secp_ctx: &self.secp_ctx,
            child_number: self.child_number.clone(),
        }
    }
}

impl<'secp, C: secp256k1::Verification> Copy for DescriptorPublicKeyCtx<'secp, C> {}

impl<'secp, C: secp256k1::Verification> DescriptorPublicKeyCtx<'secp, C> {
    /// Create a new context
    pub fn new(secp_ctx: &'secp secp256k1::Secp256k1<C>, child_number: bip32::ChildNumber) -> Self {
        Self {
            secp_ctx: secp_ctx,
            child_number: child_number,
        }
    }
}

impl<'secp, C: secp256k1::Verification> ToPublicKey<DescriptorPublicKeyCtx<'secp, C>>
    for DescriptorPublicKey
{
    fn to_public_key(&self, to_pk_ctx: DescriptorPublicKeyCtx<'secp, C>) -> bitcoin::PublicKey {
        let xpub = self.clone().derive(to_pk_ctx.child_number);
        match xpub {
            DescriptorPublicKey::SinglePub(ref spub) => spub.key.to_public_key(NullCtx),
            DescriptorPublicKey::XPub(ref xpub) => {
                // derives if wildcard, otherwise returns self
                debug_assert!(!xpub.is_wildcard);
                xpub.xkey
                    .derive_pub(to_pk_ctx.secp_ctx, &xpub.derivation_path)
                    .expect("Shouldn't fail, only normal derivations")
                    .public_key
            }
        }
    }

    fn hash_to_hash160(
        hash: &Self::Hash,
        to_pk_ctx: DescriptorPublicKeyCtx<'secp, C>,
    ) -> hash160::Hash {
        hash.to_public_key(to_pk_ctx).to_pubkeyhash()
    }
}

#[cfg(test)]
mod test {
    use super::{DescriptorKeyParseError, DescriptorPublicKey, DescriptorSecretKey};

    use bitcoin::secp256k1;

    use std::str::FromStr;

    #[test]
    fn parse_descriptor_key_errors() {
        // We refuse creating descriptors which claim to be able to derive hardened children
        let desc = "[78412e3a/44'/0'/0']xpub6ERApfZwUNrhLCkDtcHTcxd75RbzS1ed54G1LkBUHQVHQKqhMkhgbmJbZRkrgZw4koxb5JaHWkY4ALHY2grBGRjaDMzQLcgJvLJuZZvRcEL/1/42'/*";
        assert_eq!(
            DescriptorPublicKey::from_str(desc),
            Err(DescriptorKeyParseError(
                "Hardened derivation is currently not supported."
            ))
        );

        // And even if they they claim it for the wildcard!
        let desc = "[78412e3a/44'/0'/0']xpub6ERApfZwUNrhLCkDtcHTcxd75RbzS1ed54G1LkBUHQVHQKqhMkhgbmJbZRkrgZw4koxb5JaHWkY4ALHY2grBGRjaDMzQLcgJvLJuZZvRcEL/1/42/*'";
        assert_eq!(
            DescriptorPublicKey::from_str(desc),
            Err(DescriptorKeyParseError(
                "Hardened derivation is currently not supported."
            ))
        );

        // And ones with misplaced wildcard
        let desc = "[78412e3a/44'/0'/0']xpub6ERApfZwUNrhLCkDtcHTcxd75RbzS1ed54G1LkBUHQVHQKqhMkhgbmJbZRkrgZw4koxb5JaHWkY4ALHY2grBGRjaDMzQLcgJvLJuZZvRcEL/1/*/44";
        assert_eq!(
            DescriptorPublicKey::from_str(desc),
            Err(DescriptorKeyParseError(
                "\'*\' may only appear as last element in a derivation path."
            ))
        );

        // And ones with invalid fingerprints
        let desc = "[NonHexor]xpub6ERApfZwUNrhLCkDtcHTcxd75RbzS1ed54G1LkBUHQVHQKqhMkhgbmJbZRkrgZw4koxb5JaHWkY4ALHY2grBGRjaDMzQLcgJvLJuZZvRcEL/1/*";
        assert_eq!(
            DescriptorPublicKey::from_str(desc),
            Err(DescriptorKeyParseError(
                "Malformed master fingerprint, expected 8 hex chars"
            ))
        );

        // And ones with invalid xpubs..
        let desc = "[78412e3a]xpub1ed54G1LkBUHQVHQKqhMkhgbmJbZRkrgZw4koxb5JaLcgJvLJuZZvRcEL/1/*";
        assert_eq!(
            DescriptorPublicKey::from_str(desc),
            Err(DescriptorKeyParseError("Error while parsing xkey."))
        );

        // ..or invalid raw keys
        let desc = "[78412e3a]0208a117f3897c3a13c9384b8695eed98dc31bc2500feb19a1af424cd47a5d83/1/*";
        assert_eq!(
            DescriptorPublicKey::from_str(desc),
            Err(DescriptorKeyParseError(
                "Error while parsing simple public key"
            ))
        );

        // ..or invalid separators
        let desc = "[78412e3a]]03f28773c2d975288bc7d1d205c3748651b075fbc6610e58cddeeddf8f19405aa8";
        assert_eq!(
            DescriptorPublicKey::from_str(desc),
            Err(DescriptorKeyParseError(
                "Multiple \']\' in Descriptor Public Key"
            ))
        );

        // fuzzer errors
        let desc = "[11111f11]033333333333333333333333333333323333333333333333333333333433333333]]333]]3]]101333333333333433333]]]10]333333mmmm";
        assert_eq!(
            DescriptorPublicKey::from_str(desc),
            Err(DescriptorKeyParseError(
                "Multiple \']\' in Descriptor Public Key"
            ))
        );

        // fuzz failure, hybrid keys
        let desc = "0777777777777777777777777777777777777777777777777777777777777777777777777777777777777777777777777777777777777777777777777777777777";
        assert_eq!(
            DescriptorPublicKey::from_str(desc),
            Err(DescriptorKeyParseError(
                "Only publickeys with prefixes 02/03/04 are allowed"
            ))
        );
    }

    #[test]
    fn parse_descriptor_secret_key_error() {
        // Xpubs are invalid
        let secret_key = "xpub6ERApfZwUNrhLCkDtcHTcxd75RbzS1ed54G1LkBUHQVHQKqhMkhgbmJbZRkrgZw4koxb5JaHWkY4ALHY2grBGRjaDMzQLcgJvLJuZZvRcEL";
        assert_eq!(
            DescriptorSecretKey::from_str(secret_key),
            Err(DescriptorKeyParseError("Error while parsing xkey."))
        );

        // And ones with invalid fingerprints
        let desc = "[NonHexor]tprv8ZgxMBicQKsPcwcD4gSnMti126ZiETsuX7qwrtMypr6FBwAP65puFn4v6c3jrN9VwtMRMph6nyT63NrfUL4C3nBzPcduzVSuHD7zbX2JKVc/1/*";
        assert_eq!(
            DescriptorSecretKey::from_str(desc),
            Err(DescriptorKeyParseError(
                "Malformed master fingerprint, expected 8 hex chars"
            ))
        );

        // ..or invalid raw keys
        let desc = "[78412e3a]L32jTfVLei6BYTPUpwpJSkrHx8iL9GZzeErVS8y4Y/1/*";
        assert_eq!(
            DescriptorSecretKey::from_str(desc),
            Err(DescriptorKeyParseError(
                "Error while parsing a WIF private key"
            ))
        );
    }

    #[test]
    fn test_deriv_on_xprv() {
        let secp = secp256k1::Secp256k1::signing_only();

        let secret_key = DescriptorSecretKey::from_str("tprv8ZgxMBicQKsPcwcD4gSnMti126ZiETsuX7qwrtMypr6FBwAP65puFn4v6c3jrN9VwtMRMph6nyT63NrfUL4C3nBzPcduzVSuHD7zbX2JKVc/0'/1'/2").unwrap();
        let public_key = secret_key.as_public(&secp).unwrap();
        assert_eq!(public_key.to_string(), "[2cbe2a6d/0'/1']tpubDBrgjcxBxnXyL575sHdkpKohWu5qHKoQ7TJXKNrYznh5fVEGBv89hA8ENW7A8MFVpFUSvgLqc4Nj1WZcpePX6rrxviVtPowvMuGF5rdT2Vi/2");

        let secret_key = DescriptorSecretKey::from_str("tprv8ZgxMBicQKsPcwcD4gSnMti126ZiETsuX7qwrtMypr6FBwAP65puFn4v6c3jrN9VwtMRMph6nyT63NrfUL4C3nBzPcduzVSuHD7zbX2JKVc/0'/1'/2'").unwrap();
        let public_key = secret_key.as_public(&secp).unwrap();
        assert_eq!(public_key.to_string(), "[2cbe2a6d/0'/1'/2']tpubDDPuH46rv4dbFtmF6FrEtJEy1CvLZonyBoVxF6xsesHdYDdTBrq2mHhm8AbsPh39sUwL2nZyxd6vo4uWNTU9v4t893CwxjqPnwMoUACLvMV");

        let secret_key = DescriptorSecretKey::from_str("tprv8ZgxMBicQKsPcwcD4gSnMti126ZiETsuX7qwrtMypr6FBwAP65puFn4v6c3jrN9VwtMRMph6nyT63NrfUL4C3nBzPcduzVSuHD7zbX2JKVc/0/1/2").unwrap();
        let public_key = secret_key.as_public(&secp).unwrap();
        assert_eq!(public_key.to_string(), "tpubD6NzVbkrYhZ4WQdzxL7NmJN7b85ePo4p6RSj9QQHF7te2RR9iUeVSGgnGkoUsB9LBRosgvNbjRv9bcsJgzgBd7QKuxDm23ZewkTRzNSLEDr/0/1/2");

        let secret_key = DescriptorSecretKey::from_str("[aabbccdd]tprv8ZgxMBicQKsPcwcD4gSnMti126ZiETsuX7qwrtMypr6FBwAP65puFn4v6c3jrN9VwtMRMph6nyT63NrfUL4C3nBzPcduzVSuHD7zbX2JKVc/0/1/2").unwrap();
        let public_key = secret_key.as_public(&secp).unwrap();
        assert_eq!(public_key.to_string(), "[aabbccdd]tpubD6NzVbkrYhZ4WQdzxL7NmJN7b85ePo4p6RSj9QQHF7te2RR9iUeVSGgnGkoUsB9LBRosgvNbjRv9bcsJgzgBd7QKuxDm23ZewkTRzNSLEDr/0/1/2");

        let secret_key = DescriptorSecretKey::from_str("[aabbccdd/90']tprv8ZgxMBicQKsPcwcD4gSnMti126ZiETsuX7qwrtMypr6FBwAP65puFn4v6c3jrN9VwtMRMph6nyT63NrfUL4C3nBzPcduzVSuHD7zbX2JKVc/0'/1'/2").unwrap();
        let public_key = secret_key.as_public(&secp).unwrap();
        assert_eq!(public_key.to_string(), "[aabbccdd/90'/0'/1']tpubDBrgjcxBxnXyL575sHdkpKohWu5qHKoQ7TJXKNrYznh5fVEGBv89hA8ENW7A8MFVpFUSvgLqc4Nj1WZcpePX6rrxviVtPowvMuGF5rdT2Vi/2");
    }
}
