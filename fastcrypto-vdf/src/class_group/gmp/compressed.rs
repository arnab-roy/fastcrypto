// Copyright (c) 2022, Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

//! Functionality to compress/decompress and serialize/deserialize quadratic forms.

use crate::class_group::gmp::compressed::CompressedQuadraticForm::{Generator, Nontrivial, Zero};
use crate::class_group::gmp::{Discriminant, QuadraticForm};
use crate::ParameterizedGroupElement;
use fastcrypto::error::{FastCryptoError, FastCryptoResult};
use rug::integer::Order;
use rug::ops::{DivRounding, NegAssign, Pow};
use rug::{Complete, Integer};
use std::cmp::Ordering;
use std::ops::Mul;

/// A quadratic form in compressed representation. See https://eprint.iacr.org/2020/196.pdf.
#[derive(PartialEq, Eq, Debug)]
enum CompressedQuadraticForm {
    Zero(Discriminant),
    Generator(Discriminant),
    Nontrivial(CompressedFormat),
}

#[derive(PartialEq, Eq, Debug)]
struct CompressedFormat {
    a_prime: Integer,
    t_prime: Integer,
    g: Integer,
    b0: Integer,
    b_sign: bool,
    discriminant: Discriminant,
}

impl QuadraticForm {
    /// Return the length of the serialization in bytes of a quadratic form with a given discriminant
    /// length in bits.
    pub fn serialized_length(discriminant_in_bits: usize) -> usize {
        // The number of 32 bit words needed to represent the discriminant rounded up,
        ((discriminant_in_bits + 31) / 32
            * 3 // a' is two words and t' is one word. Both is divided by g, so the length of g is subtracted from both.
            + 1 // Flags for special forms (identity or generator) and the sign of b and t'.
            + 1 // The size of g - 1 = g_size.
            // Two extra bytes for g and b0 (which has the same length). Note that 2 * g_size was already counted.
            + 2) as usize
    }

    /// Serialize a quadratic form. The length of the serialization in bytes depends on the bit-length
    /// of the discriminant and may be computed using [QuadraticForm::serialized_length].
    ///
    /// The format follows that of chiavdf (see
    /// https://github.com/Chia-Network/chiavdf/blob/bcc36af3a8de4d2fcafa571602040a4ebd4bdd56/src/bqfc.c#L222-L245)
    /// if the discriminant is 1024 bits.
    pub(super) fn serialize(&self) -> Vec<u8> {
        self.compress().serialize()
    }

    /// Deserialize bytes into a quadratic form. The expected length of the serialization in bytes
    /// depends on the bit-length of the discriminant and may be computed using [CompressedQuadraticForm::serialized_length].
    ///
    /// The format follows that of chiavdf (see https://github.com/Chia-Network/chiavdf/blob/bcc36af3a8de4d2fcafa571602040a4ebd4bdd56/src/bqfc.c#L258-L287)
    /// if the discriminant is 1024 bits.
    pub fn from_bytes(bytes: &[u8], discriminant: &Discriminant) -> FastCryptoResult<Self> {
        CompressedQuadraticForm::deserialize(bytes, discriminant)?.decompress()
    }

    /// Return a compressed representation of this quadratic form. See https://eprint.iacr.org/2020/196.pdf for a definition of the compression.
    fn compress(&self) -> CompressedQuadraticForm {
        // This implementation follows https://github.com/Chia-Network/chiavdf/blob/bcc36af3a8de4d2fcafa571602040a4ebd4bdd56/src/bqfc.c#L6-L50.

        let Self {
            a,
            b,
            c: _,
            partial_gcd_limit: _,
        } = &self;

        if a == &Integer::from(1) && b == &Integer::from(1) {
            return Zero(self.discriminant());
        } else if a == &Integer::from(2) && b == &Integer::from(1) {
            return Generator(self.discriminant());
        }

        if a == b {
            return Nontrivial(CompressedFormat {
                a_prime: Integer::new(),
                t_prime: Integer::new(),
                g: Integer::new(),
                b0: Integer::new(),
                b_sign: false,
                discriminant: self.discriminant(),
            });
        }

        let b_sign = b.is_negative();
        let b_abs = b.abs_ref().complete();

        let (_, mut t_prime) = partial_xgcd(a, &b_abs).expect("a must be positive and b non-zero");
        let g = a.gcd_ref(&t_prime).complete();

        let mut b0: Integer;
        let a_prime;

        if &g == Integer::ONE {
            b0 = Integer::new();
            a_prime = a.clone();
        } else {
            a_prime = (a / &g).complete();
            t_prime /= &g;

            // Compute b / a_prime with truncation towards zero similar to mpz_tdiv_q from the GMP library.
            b0 = b_abs.div_floor(&a_prime);
            if b_sign {
                b0 = -b0;
            }
        }

        Nontrivial(CompressedFormat {
            a_prime,
            t_prime,
            g,
            b0,
            b_sign,
            discriminant: self.discriminant(),
        })
    }
}

impl CompressedQuadraticForm {
    /// Return this as an uncompressed QuadraticForm. See https://eprint.iacr.org/2020/196.pdf for a definition of the compression.
    fn decompress(&self) -> FastCryptoResult<QuadraticForm> {
        // This implementation follows https://github.com/Chia-Network/chiavdf/blob/bcc36af3a8de4d2fcafa571602040a4ebd4bdd56/src/bqfc.c#L52-L116.
        match self {
            Zero(discriminant) => Ok(QuadraticForm::zero(discriminant)),
            Generator(discriminant) => Ok(QuadraticForm::generator(discriminant)),
            Nontrivial(form) => {
                let CompressedFormat {
                    a_prime,
                    t_prime,
                    g,
                    b0,
                    b_sign,
                    discriminant,
                } = form;

                if t_prime.is_zero() {
                    return Ok(QuadraticForm::from_a_b_discriminant(
                        a_prime.clone(),
                        a_prime.clone(),
                        discriminant,
                    ));
                }

                if a_prime.is_zero() {
                    return Err(FastCryptoError::InvalidInput);
                }

                let t = if t_prime < &Integer::new() {
                    (t_prime + a_prime).complete()
                } else {
                    t_prime.clone()
                };

                let d_mod_a = discriminant.0.modulo_ref(a_prime).complete();
                let sqrt_input = Integer::from(
                    t.pow_mod_ref(&Integer::from(2), a_prime)
                        .expect("Can only fail if the exponent is negative."),
                )
                .mul(&d_mod_a)
                .modulo(a_prime);
                let sqrt = sqrt_input.sqrt_ref().complete();

                // Ensure square root is exact
                if (&sqrt).pow(2).complete() != sqrt_input {
                    return Err(FastCryptoError::InvalidInput);
                }

                let out_a = Integer::from(a_prime * g);

                let t_inv =
                    Integer::from(t.invert_ref(a_prime).ok_or(FastCryptoError::InvalidInput)?);
                let mut out_b = sqrt.mul(&t_inv).modulo(a_prime);
                if !b0.is_negative() {
                    out_b += a_prime * b0;
                }
                if *b_sign {
                    out_b.neg_assign();
                }

                Ok(QuadraticForm::from_a_b_discriminant(
                    out_a,
                    out_b,
                    discriminant,
                ))
            }
        }
    }

    /// Serialize a compressed binary form according to the format defined in the chiavdf library.
    fn serialize(&self) -> Vec<u8> {
        // This implementation follows https://github.com/Chia-Network/chiavdf/blob/bcc36af3a8de4d2fcafa571602040a4ebd4bdd56/src/bqfc.c#L222-L245.
        match self {
            Zero(d) => {
                let mut bytes = vec![0x00; QuadraticForm::serialized_length(d.bits())];
                bytes[0] = 0x04;
                bytes
            }
            Generator(d) => {
                let mut bytes = vec![0x00; QuadraticForm::serialized_length(d.bits())];
                bytes[0] = 0x08;
                bytes
            }
            Nontrivial(form) => {
                let mut bytes = vec![];
                bytes.push(form.b_sign as u8);
                bytes[0] |= ((form.t_prime < Integer::new()) as u8) << 1;

                // The bit length of the discriminant, which is rounded up to the next multiple of 32.
                // Serialization of special forms (identity or generator) takes only 1 byte.
                let d_bits = (form.discriminant.bits() + 31) & !31;

                // Size of g in bytes minus 1 (g_size)
                let g_size = (form.g.significant_bits() as usize + 7) / 8 - 1;
                bytes.push(g_size as u8);

                let a_prime_length = d_bits / 16 - g_size;
                let t_prime_length = d_bits / 32 - g_size;
                let g_length = g_size + 1;
                let b0_length = g_size + 1;

                bytes.extend_from_slice(
                    &export_to_size(&form.a_prime, a_prime_length)
                        .expect("The size bound on the discriminant ensures that this is true"),
                );
                bytes.extend_from_slice(
                    &export_to_size(&form.t_prime, t_prime_length)
                        .expect("The size bound on the discriminant ensures that this is true"),
                );
                bytes.extend_from_slice(
                    &export_to_size(&form.g, g_length)
                        .expect("The size bound on the discriminant ensures that this is true"),
                );
                bytes.extend_from_slice(
                    &export_to_size(&form.b0, b0_length)
                        .expect("The size bound on the discriminant ensures that this is true"),
                );
                bytes.extend_from_slice(&vec![
                    0u8;
                    QuadraticForm::serialized_length(
                        form.discriminant.bits()
                    ) - bytes.len()
                ]);
                bytes
            }
        }
    }

    /// Deserialize a compressed binary form according to the format defined in the chiavdf library.
    fn deserialize(bytes: &[u8], discriminant: &Discriminant) -> FastCryptoResult<Self> {
        if bytes.len() != QuadraticForm::serialized_length(discriminant.bits()) {
            return Err(FastCryptoError::InputLengthWrong(bytes.len()));
        }

        // This implementation follows https://github.com/Chia-Network/chiavdf/blob/bcc36af3a8de4d2fcafa571602040a4ebd4bdd56/src/bqfc.c#L258-L287.
        let is_identity = bytes[0] & 0x04 != 0;
        if is_identity {
            return Ok(Zero(discriminant.clone()));
        }

        let is_generator = bytes[0] & 0x08 != 0;
        if is_generator {
            return Ok(Generator(discriminant.clone()));
        }

        // The bit length of the discriminant, which is rounded up to the next multiple of 32.
        // Serialization of special forms (identity or generator) takes only 1 byte.
        let d_bits = (discriminant.bits() + 31) & !31;

        // Size of g in bytes minus 1 (g_size)
        let g_size = bytes[1] as usize;
        if g_size >= d_bits / 32 {
            return Err(FastCryptoError::InvalidInput);
        }

        let mut offset = 2;
        let a_prime_length = d_bits / 16 - g_size;
        let t_prime_length = d_bits / 32 - g_size;
        let g_length = g_size + 1;
        let b0_length = g_size + 1;

        // a' = a / g
        let a_prime = bigint_from_bytes(&bytes[offset..offset + a_prime_length]);
        offset += a_prime_length;

        // t' = t / g, where t satisfies (a*x + b*t < sqrt(a))
        let mut t_prime = bigint_from_bytes(&bytes[offset..offset + t_prime_length]);
        let t_sign = bytes[0] & 0x02 != 0;
        if t_sign {
            t_prime = -t_prime;
        }
        offset += t_prime_length;

        // g = gcd(a, t)
        let g = bigint_from_bytes(&bytes[offset..offset + g_length]);
        offset += g_length;

        // b0 = b / a'
        let b0 = bigint_from_bytes(&bytes[offset..offset + b0_length]);
        let b_sign = bytes[0] & 0x01 != 0;

        Ok(Nontrivial(CompressedFormat {
            a_prime,
            t_prime,
            g,
            b0,
            b_sign,
            discriminant: discriminant.clone(),
        }))
    }
}

/// Import function for BigInts using little-endian representation.
fn bigint_from_bytes(bytes: &[u8]) -> Integer {
    Integer::from_digits(bytes, rug::integer::Order::Lsf)
}

/// Export function for BigInts using little-endian representation.
fn bigint_to_bytes(n: &Integer) -> Vec<u8> {
    n.to_digits(Order::Lsf)
}

/// Export a curv::BigInt to a byte array of the given size. Zeroes are padded to the end if the number
/// serializes to fewer bits than `target_size`. If the serialization is too large, an error is returned.
fn export_to_size(number: &Integer, target_size: usize) -> FastCryptoResult<Vec<u8>> {
    let mut bytes = bigint_to_bytes(number);
    match bytes.len().cmp(&target_size) {
        Ordering::Less => {
            bytes.append(&mut vec![0u8; target_size - bytes.len()]);
            Ok(bytes)
        }
        Ordering::Equal => Ok(bytes),
        Ordering::Greater => Err(FastCryptoError::InputTooLong(bytes.len())),
    }
}

/// Takes `a`and `b` and returns `(s, t)` such that `s = b t (mod a)` with `0 <= s < sqrt(a) and |t|
/// <= sqrt(a)`. This is algorithm 1 from https://arxiv.org/pdf/2211.16128.pdf.
fn partial_xgcd(a: &Integer, b: &Integer) -> FastCryptoResult<(Integer, Integer)> {
    if a <= b {
        let (s, t) = partial_xgcd(b, a)?;
        return Ok((t, s));
    }

    if b <= &Integer::new() {
        return Err(FastCryptoError::InvalidInput);
    }

    let mut s = (b.clone(), a.clone());
    let mut t = (Integer::ONE.to_owned(), Integer::new());

    while s.0 >= a.sqrt_ref().complete() {
        let (q, r) = s.1.div_rem_ref(&s.0).complete();
        s.1 = s.0;
        s.0 = r;

        let t_tmp = Integer::from(&t.1 - &q * &t.0);
        t.1 = t.0;
        t.0 = t_tmp;
    }

    Ok((s.0, t.0))
}

#[cfg(test)]
mod tests {
    use crate::class_group::gmp::compressed::{
        bigint_from_bytes, bigint_to_bytes, CompressedQuadraticForm,
    };
    use crate::class_group::gmp::{Discriminant, QuadraticForm};
    use crate::ParameterizedGroupElement;
    use num_bigint::BigInt;
    use rug::Integer;
    use std::str::FromStr;

    #[test]
    fn test_bigint_import() {
        let bytes = hex::decode("0102").unwrap();
        let bigint = bigint_from_bytes(&bytes);

        // We expect little endian, e.g. 0x02 * 256 + 0x01 = 513.
        let expected = Integer::from_str_radix("513", 10).unwrap();
        assert_eq!(bigint, expected);

        let reconstructed = bigint_to_bytes(&bigint);
        assert_eq!(bytes, reconstructed);
    }

    #[test]
    fn test_compression() {
        let discriminant_hex = "d2b4bc45525b1c2b59e1ad7f81a1003f2f0efdcbc734bf711ebf5599a73577a282af5e8959ffcf3ec8601b601bcd2fa54915823d73130e90cb90fe1c6c7c10bf";
        let discriminant =
            Discriminant::try_from(-Integer::from_str_radix(discriminant_hex, 16).unwrap())
                .unwrap();
        let compressed_hex = "0200222889d197dbfddc011bba8725c753b3caf8cb85b2a03b4f8d92cf5606e81208d717f068b8476ffe1f9c2e0443fc55030605";
        let compressed = CompressedQuadraticForm::deserialize(
            &hex::decode(compressed_hex).unwrap(),
            &discriminant,
        )
        .unwrap();
        let decompressed = compressed.decompress().unwrap();
        let recompressed = decompressed.compress();
        assert_eq!(compressed, recompressed);
    }

    #[test]
    fn test_serialize_deserialize() {
        let discriminant_hex = "d2b4bc45525b1c2b59e1ad7f81a1003f2f0efdcbc734bf711ebf5599a73577a282af5e8959ffcf3ec8601b601bcd2fa54915823d73130e90cb90fe1c6c7c10bf";
        let discriminant =
            Discriminant::try_from(-Integer::from_str_radix(discriminant_hex, 16).unwrap())
                .unwrap();
        let compressed_hex = "010083b82ff747c385b0e2ff91ef1bea77d3d70b74322db1cd405e457aefece6ff23961c1243f1ed69e15efd232397e467200100";
        let compressed_bytes = hex::decode(compressed_hex).unwrap();
        let compressed =
            CompressedQuadraticForm::deserialize(&compressed_bytes, &discriminant).unwrap();
        let serialized = compressed.serialize();
        assert_eq!(serialized.to_vec(), compressed_bytes);

        let length = QuadraticForm::serialized_length(discriminant.bits());

        let mut generator_serialized = vec![0x08];
        generator_serialized.extend_from_slice(&vec![0u8; length - 1]);
        assert_eq!(
            QuadraticForm::generator(&discriminant)
                .compress()
                .serialize(),
            generator_serialized
        );
        assert_eq!(
            QuadraticForm::generator(&discriminant),
            QuadraticForm::from_bytes(&generator_serialized, &discriminant).unwrap()
        );

        let mut identity_serialized = vec![0x04];
        identity_serialized.extend_from_slice(&vec![0u8; length - 1]);
        assert_eq!(
            QuadraticForm::zero(&discriminant).compress().serialize(),
            identity_serialized
        );
        assert_eq!(
            QuadraticForm::zero(&discriminant),
            QuadraticForm::from_bytes(&identity_serialized, &discriminant).unwrap()
        );
    }

    #[test]
    fn test_serialize_roundtrip() {
        // 512, 1024, 2048 and 4096 bits
        let discriminants = [
            "-9349344414767291113687223839476811112057517254984004685948091483948469540163634423565760143454771869645957446839582874595782298614481082568123251157411687",
            "-133945061969889266637985327980602701669957743979382571436531763623415706276402737192009754195707000763534826528470478732951439968182253841713707751680514914997731717008973123373160242352119122869810833826423629802461890931457718412113596718805448770307254626415119526466550394593324563882174686655718775270447",
            "-29502142669795498170664913925261110998320411268548537483129113540779280561083683352182517520690699478273319868447448049966824511039919308043747877951680827633851250876773921459982042061851444137132714948181860869206531105248168224678068701295818400875143336452362204697641282000514554237783258014492731972413087647918643222949297880308212892726925365719811319120311399853900323484711428931751287527191097875770471316418233180621991992577566395542854095151545112408782988736372758594134766939199932173978149654618994408144132349550563062288824293800449098318712711815821352232797398061624841110469260018248562843766511",
            "-1007406630399371166205680828506843661949414311260040967856089339951193128060006822186578417382690035289449410666011850863693848919000628846349158715617084456083709831037163606319682672637324840187988607127103283149127943287978050624989555034830938436492975275987366038909474637467450001207425286269651430287955788923542179414542154414299977476302876585624737430226443723554486671958211612001960238001471273685967498771059733513459006129260882122390792571950782612040307833174744553353810400760504366039499327516985390664823589969989307911300950073410116630825901270255248406423708217095849457069056140995525605401875876118373137298999494339171538428290676256719705881706431651985194776829197614940001195992054408265445358913742096341471054976467547938020859817598310858507427495592930840526330743650698223650223475256616630888604670277950241581755495006259849435974983398554883297788462241826616412920690989472098631426747304873946834232860439878253783060639505051324901511090179582728174169603085475715057689175073017095753308275310776520002427239928789097518771962660619070493257590325261876957495417502288636882538000005279327607258660706478536265303230535024676764883243771806618176424548574077467727598718632427911394987209476759",
        ].map(|s| Integer::from_str(s).unwrap()).map(|p| Discriminant::try_from(p).unwrap());

        for discriminant in discriminants {
            let form = QuadraticForm::generator(&discriminant).mul(&BigInt::from(1234));
            let serialized = form.serialize();
            assert_eq!(
                form,
                QuadraticForm::from_bytes(&serialized, &discriminant).unwrap()
            );
        }
    }
}
