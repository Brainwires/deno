// Copyright 2018-2026 the Deno authors. MIT license.

//! The `node:crypto` ciphers beyond the ones `cipher.rs` spells out
//! (DENEXT.md item NNN): AES-192 CBC and GCM; the CFB (128, 8 and 1 bit), OFB,
//! CCM, OCB and XTS modes of AES; camellia and sm4; two- and three-key DES in
//! ECB, CBC, CFB and OFB. Each is built from RustCrypto block ciphers and
//! mode crates behind one encryptor and one decryptor interface, so
//! `cipher.rs` needs a single variant for all of them.
//!
//! The JS side (`internal/crypto/cipher.ts`) buffers whole blocks for the
//! block modes (ECB, CBC) as it does for the others, passes everything
//! through for the stream modes, and hands CCM and OCB their whole input in
//! one call: RustCrypto's CCM and OCB are one-shot AEADs.

use aes::cipher::BlockCipher;
use aes::cipher::BlockDecrypt;
use aes::cipher::BlockDecryptMut;
use aes::cipher::BlockEncrypt;
use aes::cipher::BlockEncryptMut;
use aes::cipher::BlockSizeUser;
use aes::cipher::InnerIvInit;
use aes::cipher::KeyInit;
use aes::cipher::StreamCipher;
use aes::cipher::block_padding::Pkcs7;
use aes::cipher::consts::U16;
use ccm::aead;
use ccm::aead::generic_array::ArrayLength;
use ccm::aead::generic_array::typenum::Unsigned;
use digest::generic_array::GenericArray;
use subtle::ConstantTimeEq;

use crate::cipher::CipherError;
use crate::cipher::DecipherError;

type Tag = Option<Vec<u8>>;

/// An encryptor of one of the legacy ciphers.
pub(crate) trait EncMode {
  fn set_aad(&mut self, _aad: &[u8]) {}
  /// Encrypts `input` (whole blocks for a block mode) into `output`.
  /// `false`: the input can't be taken in this state (Node's "Trying to add
  /// data in unsupported state").
  fn update(&mut self, input: &[u8], output: &mut [u8]) -> bool;
  /// Encrypts the last (partial) block, padded when `auto_pad`, and returns
  /// the authentication tag of an AEAD.
  fn finish(
    self: Box<Self>,
    auto_pad: bool,
    input: &[u8],
    output: &mut [u8],
  ) -> Result<Tag, CipherError>;
  /// The authentication tag of an AEAD, with nothing more to encrypt.
  fn take_tag(self: Box<Self>) -> Tag;
}

/// A decryptor of one of the legacy ciphers.
pub(crate) trait DecMode {
  fn set_aad(&mut self, _aad: &[u8]) {}
  /// The authentication tag, set before the data (CCM needs it then).
  fn set_auth_tag(&mut self, _tag: &[u8]) {}
  fn validate_auth_tag(&self, _length: usize) -> Result<(), DecipherError> {
    Ok(())
  }
  /// Decrypts `input` into `output`; `false` for input this state can't take
  /// or, for CCM and OCB, data that doesn't authenticate.
  fn update(&mut self, input: &[u8], output: &mut [u8]) -> bool;
  /// Decrypts the last block (its padding left for the caller to strip, as
  /// `cipher.rs` does) and checks the tag of an AEAD.
  fn finish(
    self: Box<Self>,
    auto_pad: bool,
    input: &[u8],
    output: &mut [u8],
    auth_tag: &[u8],
  ) -> Result<(), DecipherError>;
}

// ---- block modes: ECB and CBC ----

struct BlockEnc<M>(M);

impl<M: BlockEncryptMut + 'static> EncMode for BlockEnc<M> {
  fn update(&mut self, input: &[u8], output: &mut [u8]) -> bool {
    let bs = M::block_size();
    if !input.len().is_multiple_of(bs) {
      return false;
    }
    for (i, o) in input.chunks(bs).zip(output.chunks_mut(bs)) {
      self.0.encrypt_block_b2b_mut(
        GenericArray::from_slice(i),
        GenericArray::from_mut_slice(o),
      );
    }
    true
  }

  fn finish(
    self: Box<Self>,
    auto_pad: bool,
    input: &[u8],
    output: &mut [u8],
  ) -> Result<Tag, CipherError> {
    let bs = M::block_size();
    let mut mode = self.0;
    if auto_pad {
      mode
        .encrypt_padded_b2b_mut::<Pkcs7>(input, output)
        .map_err(|_| CipherError::CannotPadInputData)?;
    } else if !input.is_empty() {
      if input.len() != bs {
        return Err(CipherError::CannotPadInputData);
      }
      mode.encrypt_block_b2b_mut(
        GenericArray::from_slice(input),
        GenericArray::from_mut_slice(&mut output[..bs]),
      );
    }
    Ok(None)
  }

  fn take_tag(self: Box<Self>) -> Tag {
    None
  }
}

struct BlockDec<M>(M);

impl<M: BlockDecryptMut + 'static> DecMode for BlockDec<M> {
  fn update(&mut self, input: &[u8], output: &mut [u8]) -> bool {
    let bs = M::block_size();
    if !input.len().is_multiple_of(bs) {
      return false;
    }
    for (i, o) in input.chunks(bs).zip(output.chunks_mut(bs)) {
      self.0.decrypt_block_b2b_mut(
        GenericArray::from_slice(i),
        GenericArray::from_mut_slice(o),
      );
    }
    true
  }

  fn finish(
    self: Box<Self>,
    auto_pad: bool,
    input: &[u8],
    output: &mut [u8],
    _auth_tag: &[u8],
  ) -> Result<(), DecipherError> {
    let bs = M::block_size();
    if input.is_empty() && !auto_pad {
      return Ok(());
    }
    if input.len() != bs {
      return Err(DecipherError::InvalidFinalBlockLength);
    }
    let mut mode = self.0;
    mode.decrypt_block_b2b_mut(
      GenericArray::from_slice(input),
      GenericArray::from_mut_slice(&mut output[..bs]),
    );
    if auto_pad {
      // PKCS#7: the caller strips it; refuse a block that isn't padded
      let pad = output[bs - 1] as usize;
      if pad == 0
        || pad > bs
        || output[bs - pad..bs].iter().any(|b| *b as usize != pad)
      {
        return Err(DecipherError::CannotUnpadInputData);
      }
    }
    Ok(())
  }
}

// ---- stream modes: CTR, OFB, CFB-128, CFB-8, CFB-1 ----

/// A mode that turns any length of input into the same length of output,
/// the same way in both directions (CTR, OFB).
struct Keystream<S>(S);

impl<S: StreamCipher + 'static> EncMode for Keystream<S> {
  fn update(&mut self, input: &[u8], output: &mut [u8]) -> bool {
    self
      .0
      .apply_keystream_b2b(input, &mut output[..input.len()])
      .is_ok()
  }

  fn finish(
    self: Box<Self>,
    _auto_pad: bool,
    _input: &[u8],
    _output: &mut [u8],
  ) -> Result<Tag, CipherError> {
    Ok(None)
  }

  fn take_tag(self: Box<Self>) -> Tag {
    None
  }
}

impl<S: StreamCipher + 'static> DecMode for Keystream<S> {
  fn update(&mut self, input: &[u8], output: &mut [u8]) -> bool {
    self
      .0
      .apply_keystream_b2b(input, &mut output[..input.len()])
      .is_ok()
  }

  fn finish(
    self: Box<Self>,
    _auto_pad: bool,
    _input: &[u8],
    _output: &mut [u8],
    _auth_tag: &[u8],
  ) -> Result<(), DecipherError> {
    Ok(())
  }
}

/// CFB with a full-block feedback (`-cfb`, `-cfb128`), any input length.
struct Cfb<C: BlockEncryptMut + BlockCipher> {
  encrypt: bool,
  enc: Option<cfb_mode::BufEncryptor<C>>,
  dec: Option<cfb_mode::BufDecryptor<C>>,
}

impl<C: BlockEncryptMut + BlockCipher> Cfb<C> {
  fn apply(&mut self, input: &[u8], output: &mut [u8]) {
    let out = &mut output[..input.len()];
    out.copy_from_slice(input);
    if self.encrypt {
      self.enc.as_mut().unwrap().encrypt(out);
    } else {
      self.dec.as_mut().unwrap().decrypt(out);
    }
  }
}

/// CFB with an 8-bit feedback (`-cfb8`): one block cipher call per byte.
struct Cfb8<C: BlockEncryptMut + BlockCipher> {
  encrypt: bool,
  enc: Option<cfb8::Encryptor<C>>,
  dec: Option<cfb8::Decryptor<C>>,
}

impl<C: BlockEncryptMut + BlockCipher> Cfb8<C> {
  fn apply(&mut self, input: &[u8], output: &mut [u8]) {
    let out = &mut output[..input.len()];
    out.copy_from_slice(input);
    // its block size is one byte: whole blocks of any length
    if self.encrypt {
      self
        .enc
        .as_mut()
        .unwrap()
        .encrypt_blocks_mut(to_blocks(out));
    } else {
      self
        .dec
        .as_mut()
        .unwrap()
        .decrypt_blocks_mut(to_blocks(out));
    }
  }
}

fn to_blocks(
  data: &mut [u8],
) -> &mut [GenericArray<u8, aes::cipher::consts::U1>] {
  // SAFETY: GenericArray<u8, U1> is a transparent wrapper of [u8; 1]
  unsafe {
    std::slice::from_raw_parts_mut(data.as_mut_ptr().cast(), data.len())
  }
}

/// CFB with a 1-bit feedback (`-cfb1`), bit by bit from the most
/// significant bit of each byte, as OpenSSL does. No RustCrypto crate has it;
/// it is the block cipher in CFB with a one-bit shift register.
struct Cfb1<C> {
  cipher: C,
  register: Vec<u8>,
  encrypt: bool,
}

impl<C: BlockEncrypt> Cfb1<C> {
  fn apply(&mut self, input: &[u8], output: &mut [u8]) {
    let n = self.register.len();
    let mut block = GenericArray::<u8, C::BlockSize>::default();
    for (i, byte) in input.iter().enumerate() {
      let mut out = 0u8;
      for bit in (0..8).rev() {
        block.copy_from_slice(&self.register);
        self.cipher.encrypt_block(&mut block);
        let in_bit = (byte >> bit) & 1;
        let out_bit = in_bit ^ (block[0] >> 7);
        out |= out_bit << bit;
        // the ciphertext bit feeds back
        let feedback = if self.encrypt { out_bit } else { in_bit };
        for j in 0..n {
          let next = if j + 1 < n {
            self.register[j + 1] >> 7
          } else {
            feedback
          };
          self.register[j] = (self.register[j] << 1) | next;
        }
      }
      output[i] = out;
    }
  }
}

macro_rules! stream_both {
  ($ty:ident) => {
    impl<C: BlockEncryptMut + BlockCipher + 'static> EncMode for $ty<C> {
      fn update(&mut self, input: &[u8], output: &mut [u8]) -> bool {
        self.apply(input, output);
        true
      }
      fn finish(
        self: Box<Self>,
        _auto_pad: bool,
        _input: &[u8],
        _output: &mut [u8],
      ) -> Result<Tag, CipherError> {
        Ok(None)
      }
      fn take_tag(self: Box<Self>) -> Tag {
        None
      }
    }
    impl<C: BlockEncryptMut + BlockCipher + 'static> DecMode for $ty<C> {
      fn update(&mut self, input: &[u8], output: &mut [u8]) -> bool {
        self.apply(input, output);
        true
      }
      fn finish(
        self: Box<Self>,
        _auto_pad: bool,
        _input: &[u8],
        _output: &mut [u8],
        _auth_tag: &[u8],
      ) -> Result<(), DecipherError> {
        Ok(())
      }
    }
  };
}
stream_both!(Cfb);
stream_both!(Cfb8);

impl<C: BlockEncrypt + 'static> EncMode for Cfb1<C> {
  fn update(&mut self, input: &[u8], output: &mut [u8]) -> bool {
    self.apply(input, output);
    true
  }
  fn finish(
    self: Box<Self>,
    _auto_pad: bool,
    _input: &[u8],
    _output: &mut [u8],
  ) -> Result<Tag, CipherError> {
    Ok(None)
  }
  fn take_tag(self: Box<Self>) -> Tag {
    None
  }
}

impl<C: BlockEncrypt + 'static> DecMode for Cfb1<C> {
  fn update(&mut self, input: &[u8], output: &mut [u8]) -> bool {
    self.apply(input, output);
    true
  }
  fn finish(
    self: Box<Self>,
    _auto_pad: bool,
    _input: &[u8],
    _output: &mut [u8],
    _auth_tag: &[u8],
  ) -> Result<(), DecipherError> {
    Ok(())
  }
}

// ---- XTS: each update is one data unit, the IV its tweak ----

struct Xts<C: BlockEncrypt + BlockDecrypt + BlockCipher> {
  xts: xts_mode::Xts128<C>,
  tweak: [u8; 16],
  encrypt: bool,
}

impl<C: BlockEncrypt + BlockDecrypt + BlockCipher + 'static> Xts<C> {
  fn apply(&mut self, input: &[u8], output: &mut [u8]) -> bool {
    // a data unit is at least one block (ciphertext stealing after that)
    if input.len() < 16 {
      return false;
    }
    let out = &mut output[..input.len()];
    out.copy_from_slice(input);
    if self.encrypt {
      self.xts.encrypt_sector(out, self.tweak);
    } else {
      self.xts.decrypt_sector(out, self.tweak);
    }
    true
  }
}

impl<C: BlockEncrypt + BlockDecrypt + BlockCipher + 'static> EncMode
  for Xts<C>
{
  fn update(&mut self, input: &[u8], output: &mut [u8]) -> bool {
    self.apply(input, output)
  }
  fn finish(
    self: Box<Self>,
    _auto_pad: bool,
    _input: &[u8],
    _output: &mut [u8],
  ) -> Result<Tag, CipherError> {
    Ok(None)
  }
  fn take_tag(self: Box<Self>) -> Tag {
    None
  }
}

impl<C: BlockEncrypt + BlockDecrypt + BlockCipher + 'static> DecMode
  for Xts<C>
{
  fn update(&mut self, input: &[u8], output: &mut [u8]) -> bool {
    self.apply(input, output)
  }
  fn finish(
    self: Box<Self>,
    _auto_pad: bool,
    _input: &[u8],
    _output: &mut [u8],
    _auth_tag: &[u8],
  ) -> Result<(), DecipherError> {
    Ok(())
  }
}

// ---- GCM (AES-192; AES-128 and -256 are in cipher.rs) ----

fn is_valid_gcm_tag_length(tag_len: usize) -> bool {
  tag_len == 4 || tag_len == 8 || (12..=16).contains(&tag_len)
}

struct Gcm<A: BlockCipher + BlockSizeUser<BlockSize = U16> + BlockEncrypt> {
  gcm: aead_gcm_stream::AesGcm<A>,
  tag_len: Option<usize>,
}

impl<A> EncMode for Gcm<A>
where
  A: BlockCipher
    + BlockSizeUser<BlockSize = U16>
    + BlockEncrypt
    + KeyInit
    + 'static,
{
  fn set_aad(&mut self, aad: &[u8]) {
    self.gcm.set_aad(aad);
  }
  fn update(&mut self, input: &[u8], output: &mut [u8]) -> bool {
    output[..input.len()].copy_from_slice(input);
    self.gcm.encrypt(&mut output[..input.len()]);
    true
  }
  fn finish(
    self: Box<Self>,
    _auto_pad: bool,
    _input: &[u8],
    _output: &mut [u8],
  ) -> Result<Tag, CipherError> {
    Ok(self.take_tag())
  }
  fn take_tag(self: Box<Self>) -> Tag {
    let mut tag = self.gcm.finish().to_vec();
    if let Some(len) = self.tag_len {
      tag.truncate(len);
    }
    Some(tag)
  }
}

impl<A> DecMode for Gcm<A>
where
  A: BlockCipher
    + BlockSizeUser<BlockSize = U16>
    + BlockEncrypt
    + KeyInit
    + 'static,
{
  fn set_aad(&mut self, aad: &[u8]) {
    self.gcm.set_aad(aad);
  }
  fn validate_auth_tag(&self, length: usize) -> Result<(), DecipherError> {
    match self.tag_len {
      Some(len) if len != length => Err(DecipherError::InvalidAuthTag(length)),
      None if !is_valid_gcm_tag_length(length) => {
        Err(DecipherError::InvalidAuthTag(length))
      }
      _ => Ok(()),
    }
  }
  fn update(&mut self, input: &[u8], output: &mut [u8]) -> bool {
    output[..input.len()].copy_from_slice(input);
    self.gcm.decrypt(&mut output[..input.len()]);
    true
  }
  fn finish(
    self: Box<Self>,
    _auto_pad: bool,
    _input: &[u8],
    _output: &mut [u8],
    auth_tag: &[u8],
  ) -> Result<(), DecipherError> {
    let tag = self.gcm.finish();
    let tag = match self.tag_len {
      Some(len) => &tag[..len],
      None => &tag[..],
    };
    if !auth_tag.is_empty() && bool::from(tag.ct_eq(auth_tag)) {
      Ok(())
    } else {
      Err(DecipherError::DataAuthenticationFailed)
    }
  }
}

// ---- CCM and OCB: one-shot AEADs over the whole message ----

/// One-shot AEAD over a key, a nonce and a tag length chosen at run time.
trait OneShot {
  fn seal(&self, nonce: &[u8], aad: &[u8], data: &mut [u8]) -> Vec<u8>;
  fn open(&self, nonce: &[u8], aad: &[u8], data: &mut [u8], tag: &[u8])
  -> bool;
}

struct AeadImpl<A>(A);

impl<A: aead::AeadInPlace> OneShot for AeadImpl<A> {
  fn seal(&self, nonce: &[u8], aad: &[u8], data: &mut [u8]) -> Vec<u8> {
    self
      .0
      .encrypt_in_place_detached(GenericArray::from_slice(nonce), aad, data)
      .map(|t| t.to_vec())
      .unwrap_or_default()
  }
  fn open(
    &self,
    nonce: &[u8],
    aad: &[u8],
    data: &mut [u8],
    tag: &[u8],
  ) -> bool {
    if tag.len() != A::TagSize::to_usize() {
      return false;
    }
    self
      .0
      .decrypt_in_place_detached(
        GenericArray::from_slice(nonce),
        aad,
        data,
        GenericArray::from_slice(tag),
      )
      .is_ok()
  }
}

macro_rules! ccm_dispatch {
  ($c:ty, $key:expr, $n:expr, $m:expr) => {{
    use aes::cipher::consts::*;
    macro_rules! with_m {
      ($nn:ty) => {
        match $m {
          4 => ccm_new::<$c, U4, $nn>($key),
          6 => ccm_new::<$c, U6, $nn>($key),
          8 => ccm_new::<$c, U8, $nn>($key),
          10 => ccm_new::<$c, U10, $nn>($key),
          12 => ccm_new::<$c, U12, $nn>($key),
          14 => ccm_new::<$c, U14, $nn>($key),
          16 => ccm_new::<$c, U16, $nn>($key),
          _ => None,
        }
      };
    }
    match $n {
      7 => with_m!(U7),
      8 => with_m!(U8),
      9 => with_m!(U9),
      10 => with_m!(U10),
      11 => with_m!(U11),
      12 => with_m!(U12),
      13 => with_m!(U13),
      _ => None,
    }
  }};
}

fn ccm_new<C, M, N>(key: &[u8]) -> Option<Box<dyn OneShot>>
where
  C: BlockCipher
    + BlockSizeUser<BlockSize = U16>
    + BlockEncrypt
    + KeyInit
    + 'static,
  M: ArrayLength<u8> + ccm::TagSize + 'static,
  N: ArrayLength<u8> + ccm::NonceSize + 'static,
  ccm::Ccm<C, M, N>: aead::AeadInPlace + KeyInit,
{
  let cipher = ccm::Ccm::<C, M, N>::new_from_slice(key).ok()?;
  Some(Box::new(AeadImpl(cipher)))
}

macro_rules! ocb_dispatch {
  ($c:ty, $key:expr, $n:expr, $m:expr) => {{
    use aes::cipher::consts::*;
    macro_rules! with_m {
      ($nn:ty) => {
        match $m {
          8 => ocb_new::<$c, $nn, U8>($key),
          12 => ocb_new::<$c, $nn, U12>($key),
          16 => ocb_new::<$c, $nn, U16>($key),
          _ => None,
        }
      };
    }
    match $n {
      6 => with_m!(U6),
      7 => with_m!(U7),
      8 => with_m!(U8),
      9 => with_m!(U9),
      10 => with_m!(U10),
      11 => with_m!(U11),
      12 => with_m!(U12),
      13 => with_m!(U13),
      14 => with_m!(U14),
      15 => with_m!(U15),
      _ => None,
    }
  }};
}

fn ocb_new<C, N, M>(key: &[u8]) -> Option<Box<dyn OneShot>>
where
  C: BlockCipher
    + BlockSizeUser<BlockSize = U16>
    + BlockEncrypt
    + BlockDecrypt
    + KeyInit
    + 'static,
  ocb3::Ocb3<C, N, M>: aead::AeadInPlace + KeyInit + 'static,
{
  let cipher = ocb3::Ocb3::<C, N, M>::new_from_slice(key).ok()?;
  Some(Box::new(AeadImpl(cipher)))
}

struct OneShotMode {
  aead: Box<dyn OneShot>,
  nonce: Vec<u8>,
  aad: Vec<u8>,
  /// CCM takes exactly one update (Node: "Trying to add data in
  /// unsupported state" for a second one).
  done: bool,
  tag: Option<Vec<u8>>,
  failed: bool,
}

impl EncMode for OneShotMode {
  fn set_aad(&mut self, aad: &[u8]) {
    self.aad.extend_from_slice(aad);
  }
  fn update(&mut self, input: &[u8], output: &mut [u8]) -> bool {
    if self.done {
      return false;
    }
    self.done = true;
    let out = &mut output[..input.len()];
    out.copy_from_slice(input);
    self.tag = Some(self.aead.seal(&self.nonce, &self.aad, out));
    true
  }
  fn finish(
    self: Box<Self>,
    _auto_pad: bool,
    _input: &[u8],
    _output: &mut [u8],
  ) -> Result<Tag, CipherError> {
    Ok(self.take_tag())
  }
  fn take_tag(self: Box<Self>) -> Tag {
    match self.tag {
      Some(tag) => Some(tag),
      // nothing encrypted: the tag of the empty message
      None => Some(self.aead.seal(&self.nonce, &self.aad, &mut [])),
    }
  }
}

impl DecMode for OneShotMode {
  fn set_aad(&mut self, aad: &[u8]) {
    self.aad.extend_from_slice(aad);
  }
  fn set_auth_tag(&mut self, tag: &[u8]) {
    self.tag = Some(tag.to_vec());
  }
  fn update(&mut self, input: &[u8], output: &mut [u8]) -> bool {
    if self.done {
      return false;
    }
    self.done = true;
    let out = &mut output[..input.len()];
    out.copy_from_slice(input);
    let tag = self.tag.clone().unwrap_or_default();
    let ok = self.aead.open(&self.nonce, &self.aad, out, &tag);
    if !ok {
      out.fill(0);
      self.failed = true;
    }
    ok
  }
  fn finish(
    self: Box<Self>,
    _auto_pad: bool,
    _input: &[u8],
    _output: &mut [u8],
    _auth_tag: &[u8],
  ) -> Result<(), DecipherError> {
    if self.failed {
      return Err(DecipherError::DataAuthenticationFailed);
    }
    if !self.done {
      // nothing decrypted: the tag of the empty message
      let tag = self.tag.clone().unwrap_or_default();
      if !self.aead.open(&self.nonce, &self.aad, &mut [], &tag) {
        return Err(DecipherError::DataAuthenticationFailed);
      }
    }
    Ok(())
  }
}

// ---- names ----

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Algo {
  Aes128,
  Aes192,
  Aes256,
  Camellia128,
  Camellia192,
  Camellia256,
  Sm4,
  DesEde2,
  DesEde3,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
  Ecb,
  Cbc,
  Ctr,
  Cfb,
  Cfb8,
  Cfb1,
  Ofb,
  Gcm,
  Ccm,
  Ocb,
  Xts,
}

/// The cipher and mode of a name `getCiphers()` reports for this module.
fn parse(name: &str) -> Option<(Algo, Mode)> {
  use Algo::*;
  use Mode::*;
  let alias = match name {
    "aes192" => "aes-192-cbc",
    "camellia128" => "camellia-128-cbc",
    "camellia192" => "camellia-192-cbc",
    "camellia256" => "camellia-256-cbc",
    "sm4" => "sm4-cbc",
    "sm4-cfb128" => "sm4-cfb",
    "sm4-ofb128" => "sm4-ofb",
    "des-ede" | "des-ede-ecb" => "des-ede-ecb",
    "des-ede3" | "des-ede3-ecb" => "des-ede3-ecb",
    "des3" => "des-ede3-cbc",
    "id-aes128-gcm" => "aes-128-gcm",
    "id-aes192-gcm" => "aes-192-gcm",
    "id-aes256-gcm" => "aes-256-gcm",
    "id-aes128-ccm" => "aes-128-ccm",
    "id-aes192-ccm" => "aes-192-ccm",
    "id-aes256-ccm" => "aes-256-ccm",
    other => other,
  };
  let (algo, mode) = if let Some(rest) = alias.strip_prefix("sm4-") {
    (Sm4, rest)
  } else if let Some(rest) = alias.strip_prefix("des-ede3-") {
    (DesEde3, rest)
  } else if let Some(rest) = alias.strip_prefix("des-ede-") {
    (DesEde2, rest)
  } else {
    let (family, rest) = alias.split_once('-')?;
    let (bits, mode) = rest.split_once('-')?;
    let algo = match (family, bits) {
      ("aes", "128") => Aes128,
      ("aes", "192") => Aes192,
      ("aes", "256") => Aes256,
      ("camellia", "128") => Camellia128,
      ("camellia", "192") => Camellia192,
      ("camellia", "256") => Camellia256,
      _ => return None,
    };
    (algo, mode)
  };
  let mode = match mode {
    "ecb" => Ecb,
    "cbc" => Cbc,
    "ctr" => Ctr,
    "cfb" => Cfb,
    "cfb8" => Cfb8,
    "cfb1" => Cfb1,
    "ofb" => Ofb,
    "gcm" => Gcm,
    "ccm" => Ccm,
    "ocb" => Ocb,
    "xts" => Xts,
    _ => return None,
  };
  let aes = matches!(algo, Aes128 | Aes192 | Aes256);
  let des = matches!(algo, DesEde2 | DesEde3);
  let ok = match mode {
    Ecb | Cbc | Cfb | Ofb => true,
    Ctr | Cfb8 | Cfb1 => !des || (algo == DesEde3 && mode != Ctr),
    Gcm | Ccm | Ocb => aes,
    Xts => matches!(algo, Aes128 | Aes256),
  };
  ok.then_some((algo, mode))
}

/// Whether the name is one this module serves.
pub(crate) fn is_legacy(name: &str) -> bool {
  parse(name).is_some()
}

fn key_len(algo: Algo, mode: Mode) -> usize {
  let base = match algo {
    Algo::Aes128 | Algo::Camellia128 | Algo::Sm4 | Algo::DesEde2 => 16,
    Algo::Aes192 | Algo::Camellia192 | Algo::DesEde3 => 24,
    Algo::Aes256 | Algo::Camellia256 => 32,
  };
  if mode == Mode::Xts { base * 2 } else { base }
}

fn block_len(algo: Algo) -> usize {
  match algo {
    Algo::DesEde2 | Algo::DesEde3 => 8,
    _ => 16,
  }
}

/// Key and IV checks shared by both directions.
#[derive(Debug)]
enum ParamError {
  Key,
  Iv,
  Tag(usize),
}

fn check(
  algo: Algo,
  mode: Mode,
  key: &[u8],
  iv: &[u8],
  tag_len: Option<usize>,
) -> Result<(), ParamError> {
  if key.len() != key_len(algo, mode) {
    return Err(ParamError::Key);
  }
  let iv_ok = match mode {
    Mode::Ecb => iv.is_empty(),
    Mode::Gcm => !iv.is_empty(),
    Mode::Ccm => (7..=13).contains(&iv.len()),
    Mode::Ocb => (6..=15).contains(&iv.len()),
    _ => iv.len() == block_len(algo),
  };
  if !iv_ok {
    return Err(ParamError::Iv);
  }
  match (mode, tag_len) {
    (Mode::Gcm, Some(t)) if !is_valid_gcm_tag_length(t) => {
      Err(ParamError::Tag(t))
    }
    (Mode::Ccm, Some(t)) if !(4..=16).contains(&t) || t % 2 != 0 => {
      Err(ParamError::Tag(t))
    }
    (Mode::Ocb, Some(t)) if ![8, 12, 16].contains(&t) => {
      Err(ParamError::Tag(t))
    }
    (Mode::Ccm | Mode::Ocb, None) => Err(ParamError::Tag(0)),
    _ => Ok(()),
  }
}

/// The modes every block cipher here has, for one direction.
fn basic<C>(mode: Mode, key: &[u8], iv: &[u8], encrypt: bool) -> Option<Both>
where
  C: BlockCipher
    + BlockEncrypt
    + BlockDecrypt
    + BlockEncryptMut
    + BlockDecryptMut
    + KeyInit
    + Clone
    + 'static,
{
  let cipher = C::new_from_slice(key).ok()?;
  let iv_block = || GenericArray::from_slice(iv);
  Some(match (mode, encrypt) {
    (Mode::Ecb, true) => Both::Enc(Box::new(BlockEnc(
      <ecb::Encryptor<C> as aes::cipher::InnerInit>::inner_init(cipher),
    ))),
    (Mode::Ecb, false) => Both::Dec(Box::new(BlockDec(
      <ecb::Decryptor<C> as aes::cipher::InnerInit>::inner_init(cipher),
    ))),
    (Mode::Cbc, true) => Both::Enc(Box::new(BlockEnc(
      cbc::Encryptor::<C>::inner_iv_init(cipher, iv_block()),
    ))),
    (Mode::Cbc, false) => Both::Dec(Box::new(BlockDec(
      cbc::Decryptor::<C>::inner_iv_init(cipher, iv_block()),
    ))),
    (Mode::Cfb, encrypt) => {
      let mode = Cfb::<C> {
        encrypt,
        enc: encrypt.then(|| {
          cfb_mode::BufEncryptor::inner_iv_init(cipher.clone(), iv_block())
        }),
        dec: (!encrypt)
          .then(|| cfb_mode::BufDecryptor::inner_iv_init(cipher, iv_block())),
      };
      Both::boxed(mode, encrypt)
    }
    (Mode::Cfb8, encrypt) => {
      let mode = Cfb8::<C> {
        encrypt,
        enc: encrypt
          .then(|| cfb8::Encryptor::inner_iv_init(cipher.clone(), iv_block())),
        dec: (!encrypt)
          .then(|| cfb8::Decryptor::inner_iv_init(cipher, iv_block())),
      };
      Both::boxed(mode, encrypt)
    }
    (Mode::Cfb1, encrypt) => Both::boxed(
      Cfb1 {
        cipher,
        register: iv.to_vec(),
        encrypt,
      },
      encrypt,
    ),
    (Mode::Ofb, encrypt) => Both::boxed(
      Keystream(ofb::Ofb::<C>::inner_iv_init(cipher, iv_block())),
      encrypt,
    ),
    _ => return None,
  })
}

/// The modes of 128-bit block ciphers only.
fn wide<C>(mode: Mode, key: &[u8], iv: &[u8], encrypt: bool) -> Option<Both>
where
  C: BlockCipher
    + BlockSizeUser<BlockSize = U16>
    + BlockEncrypt
    + BlockDecrypt
    + BlockEncryptMut
    + BlockDecryptMut
    + KeyInit
    + Clone
    + 'static,
{
  match mode {
    Mode::Ctr => {
      let cipher = C::new_from_slice(key).ok()?;
      Some(Both::boxed(
        Keystream(ctr::Ctr128BE::<C>::inner_iv_init(
          cipher,
          GenericArray::from_slice(iv),
        )),
        encrypt,
      ))
    }
    Mode::Xts => {
      let (k1, k2) = key.split_at(key.len() / 2);
      let mut tweak = [0u8; 16];
      tweak.copy_from_slice(iv);
      Some(Both::boxed(
        Xts {
          xts: xts_mode::Xts128::new(
            C::new_from_slice(k1).ok()?,
            C::new_from_slice(k2).ok()?,
          ),
          tweak,
          encrypt,
        },
        encrypt,
      ))
    }
    _ => basic::<C>(mode, key, iv, encrypt),
  }
}

enum Both {
  Enc(Box<dyn EncMode>),
  Dec(Box<dyn DecMode>),
}

impl Both {
  fn boxed<T: EncMode + DecMode + 'static>(mode: T, encrypt: bool) -> Self {
    if encrypt {
      Both::Enc(Box::new(mode))
    } else {
      Both::Dec(Box::new(mode))
    }
  }
}

fn build(
  name: &str,
  key: &[u8],
  iv: &[u8],
  tag_len: Option<usize>,
  encrypt: bool,
) -> Option<Result<Both, ParamError>> {
  let (algo, mode) = parse(name)?;
  if let Err(err) = check(algo, mode, key, iv, tag_len) {
    return Some(Err(err));
  }
  let built = match (algo, mode) {
    (Algo::Aes192, Mode::Gcm) => {
      let gcm = Gcm {
        gcm: aead_gcm_stream::AesGcm::<aes::Aes192>::new(
          GenericArray::from_slice(key),
          iv,
        ),
        tag_len,
      };
      Some(Both::boxed(gcm, encrypt))
    }
    (_, Mode::Gcm) => None, // AES-128 / -256 GCM are in cipher.rs
    (_, Mode::Ccm | Mode::Ocb) => {
      let m = tag_len.unwrap_or(16);
      let n = iv.len();
      let aead = match (algo, mode) {
        (Algo::Aes128, Mode::Ccm) => ccm_dispatch!(aes::Aes128, key, n, m),
        (Algo::Aes192, Mode::Ccm) => ccm_dispatch!(aes::Aes192, key, n, m),
        (Algo::Aes256, Mode::Ccm) => ccm_dispatch!(aes::Aes256, key, n, m),
        (Algo::Aes128, Mode::Ocb) => ocb_dispatch!(aes::Aes128, key, n, m),
        (Algo::Aes192, Mode::Ocb) => ocb_dispatch!(aes::Aes192, key, n, m),
        (Algo::Aes256, Mode::Ocb) => ocb_dispatch!(aes::Aes256, key, n, m),
        _ => None,
      };
      aead.map(|aead| {
        Both::boxed(
          OneShotMode {
            aead,
            nonce: iv.to_vec(),
            aad: Vec::new(),
            done: false,
            tag: None,
            failed: false,
          },
          encrypt,
        )
      })
    }
    (Algo::DesEde2, _) => basic::<des::TdesEde2>(mode, key, iv, encrypt),
    (Algo::DesEde3, _) => basic::<des::TdesEde3>(mode, key, iv, encrypt),
    (Algo::Aes128, _) => wide::<aes::Aes128>(mode, key, iv, encrypt),
    (Algo::Aes192, _) => wide::<aes::Aes192>(mode, key, iv, encrypt),
    (Algo::Aes256, _) => wide::<aes::Aes256>(mode, key, iv, encrypt),
    (Algo::Camellia128, _) => {
      wide::<camellia::Camellia128>(mode, key, iv, encrypt)
    }
    (Algo::Camellia192, _) => {
      wide::<camellia::Camellia192>(mode, key, iv, encrypt)
    }
    (Algo::Camellia256, _) => {
      wide::<camellia::Camellia256>(mode, key, iv, encrypt)
    }
    (Algo::Sm4, _) => wide::<sm4::Sm4>(mode, key, iv, encrypt),
  };
  built.map(Ok)
}

/// The encryptor for `name`, or `None` when it isn't one of these ciphers.
pub(crate) fn new_encryptor(
  name: &str,
  key: &[u8],
  iv: &[u8],
  tag_len: Option<usize>,
) -> Option<Result<Box<dyn EncMode>, CipherError>> {
  Some(match build(name, key, iv, tag_len, true)? {
    Ok(Both::Enc(mode)) => Ok(mode),
    Ok(Both::Dec(_)) => unreachable!(),
    Err(ParamError::Key) => Err(CipherError::InvalidKeyLength),
    Err(ParamError::Iv) => Err(CipherError::InvalidInitializationVector),
    Err(ParamError::Tag(t)) => Err(CipherError::InvalidAuthTag(t)),
  })
}

/// The decryptor for `name`, or `None` when it isn't one of these ciphers.
pub(crate) fn new_decryptor(
  name: &str,
  key: &[u8],
  iv: &[u8],
  tag_len: Option<usize>,
) -> Option<Result<Box<dyn DecMode>, DecipherError>> {
  Some(match build(name, key, iv, tag_len, false)? {
    Ok(Both::Dec(mode)) => Ok(mode),
    Ok(Both::Enc(_)) => unreachable!(),
    Err(ParamError::Key) => Err(DecipherError::InvalidKeyLength),
    Err(ParamError::Iv) => Err(DecipherError::InvalidInitializationVector),
    Err(ParamError::Tag(t)) => Err(DecipherError::InvalidAuthTag(t)),
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  fn hex(s: &str) -> Vec<u8> {
    data_encoding::HEXLOWER.decode(s.as_bytes()).unwrap()
  }

  fn encrypt(name: &str, key: &[u8], iv: &[u8], data: &[u8]) -> Vec<u8> {
    let mut enc = new_encryptor(name, key, iv, None).unwrap().unwrap();
    let mut out = vec![0; data.len()];
    assert!(enc.update(data, &mut out));
    out
  }

  fn decrypt(name: &str, key: &[u8], iv: &[u8], data: &[u8]) -> Vec<u8> {
    let mut dec = new_decryptor(name, key, iv, None).unwrap().unwrap();
    let mut out = vec![0; data.len()];
    assert!(dec.update(data, &mut out));
    out
  }

  // NIST SP 800-38A, F.3 (CFB) and F.4 (OFB), first block(s), AES-128
  #[test]
  fn sp800_38a_vectors() {
    let key = hex("2b7e151628aed2a6abf7158809cf4f3c");
    let iv = hex("000102030405060708090a0b0c0d0e0f");
    let pt = hex("6bc1bee22e409f96e93d7e117393172a");
    assert_eq!(
      encrypt("aes-128-cfb", &key, &iv, &pt),
      hex("3b3fd92eb72dad20333449f8e83cfb4a")
    );
    assert_eq!(
      encrypt("aes-128-ofb", &key, &iv, &pt),
      hex("3b3fd92eb72dad20333449f8e83cfb4a")
    );
    // F.3.7 CFB8: the first 2 bytes
    assert_eq!(encrypt("aes-128-cfb8", &key, &iv, &pt[..2]), hex("3b79"));
    // F.3.1 CFB1: plaintext bits 0110 1011 1100 0001 -> 0110 1000 1011 0011
    assert_eq!(encrypt("aes-128-cfb1", &key, &iv, &pt[..2]), hex("68b3"));
    for name in ["aes-128-cfb", "aes-128-cfb8", "aes-128-cfb1", "aes-128-ofb"] {
      let ct = encrypt(name, &key, &iv, &pt);
      assert_eq!(decrypt(name, &key, &iv, &ct), pt, "{name}");
    }
  }

  #[test]
  fn names() {
    for name in [
      "aes-192-cbc",
      "aes192",
      "aes-192-gcm",
      "aes-256-xts",
      "aes-128-ocb",
      "aes-192-ccm",
      "camellia-256-cfb1",
      "camellia128",
      "sm4",
      "sm4-ofb128",
      "des-ede3",
      "des-ede-cbc",
      "des3",
      "id-aes192-ccm",
    ] {
      assert!(is_legacy(name), "{name}");
    }
    for name in [
      "aes-192-xts",
      "sm4-ccm",
      "des-ede-ctr",
      "camellia-128-gcm",
      "des-cbc",
      "bf-cbc",
      "rc4",
    ] {
      assert!(!is_legacy(name), "{name}");
    }
  }
}
