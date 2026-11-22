//! A bounded look at the FAT layout of an OLE compound file before `cfb`
//! opens it.
//!
//! cfb 0.16 reads one whole sector for every FAT-sector index the DIFAT
//! lists and only then checks the result. It does not reject an index listed
//! twice, so a DIFAT that repeats one index makes it allocate up to 128 times
//! the file size (one 512-byte sector per 4-byte entry).
//! [`check_fat_sectors`] walks the same chain in time linear in the file size
//! and rejects repeated indices; with every index distinct and inside the
//! file, cfb reads no more than the file holds. Anything else that is
//! malformed is left to cfb to report.

use crate::domain::error::DomainError;
use crate::infra::parsers::le_bytes::u32_at;

const HEADER_LEN: usize = 512;
const MAGIC: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
/// FAT-sector indices held in the header itself.
const HEADER_DIFAT_ENTRIES: usize = 109;
const HEADER_DIFAT_OFFSET: usize = 76;
const MAX_REGULAR_SECTOR: u32 = 0xFFFF_FFFA;
const END_OF_CHAIN: u32 = 0xFFFF_FFFE;
const FREE_SECTOR: u32 = 0xFFFF_FFFF;

/// The header fields the DIFAT walk needs.
struct Layout<'a> {
    bytes: &'a [u8],
    sector_len: usize,
    /// Sector indices one sector holds.
    per_sector: usize,
    /// Sectors after the header, counted like cfb (a partial one counts).
    num_sectors: u32,
    num_fat_sectors: u32,
    first_difat_sector: u32,
}

impl<'a> Layout<'a> {
    fn parse(bytes: &'a [u8]) -> Option<Self> {
        if bytes.len() < HEADER_LEN || bytes.get(..8)? != MAGIC {
            return None;
        }
        let shift = u16::from_le_bytes([*bytes.get(30)?, *bytes.get(31)?]);
        let (sector_len, per_sector) = match shift {
            9 => (512, 128),
            12 => (4096, 1024),
            _ => return None,
        };
        if bytes.len() < sector_len {
            return None;
        }
        let num_sectors = u32::try_from(bytes.len().div_ceil(sector_len) - 1).ok()?;
        let first_difat_sector = match u32_at(bytes, 68)? {
            // Some writers end the chain with FREE_SECTOR; cfb accepts it.
            FREE_SECTOR => END_OF_CHAIN,
            other => other,
        };
        Some(Self {
            bytes,
            sector_len,
            per_sector,
            num_sectors,
            num_fat_sectors: u32_at(bytes, 44)?,
            first_difat_sector,
        })
    }

    /// Calls `f` with every DIFAT entry in cfb's order: the header's 109,
    /// then all but the last entry of each DIFAT sector (the last links the
    /// chain). `None` when cfb would reject the DIFAT itself.
    fn for_each_entry(&self, mut f: impl FnMut(u32)) -> Option<()> {
        let mut free_seen = false;
        for i in 0..HEADER_DIFAT_ENTRIES {
            let entry = u32_at(self.bytes, HEADER_DIFAT_OFFSET + 4 * i)?;
            // cfb stops reading the header's list at the first free entry.
            free_seen |= entry == FREE_SECTOR;
            if free_seen {
                f(FREE_SECTOR);
            } else if entry > MAX_REGULAR_SECTOR {
                return None;
            } else {
                f(entry);
            }
        }
        let per_sector = self.per_sector;
        let mut sector = self.first_difat_sector;
        // cfb rejects a chain that visits a sector twice; more steps than
        // sectors means it did, so stop and let it.
        let mut steps = 0u32;
        while sector != END_OF_CHAIN && sector != FREE_SECTOR {
            if sector >= self.num_sectors || steps >= self.num_sectors {
                return None;
            }
            steps += 1;
            let start = (sector as usize + 1).checked_mul(self.sector_len)?;
            let data = self.bytes.get(start..start.checked_add(self.sector_len)?)?;
            for i in 0..per_sector - 1 {
                let entry = u32_at(data, 4 * i)?;
                if entry != FREE_SECTOR && entry > MAX_REGULAR_SECTOR {
                    return None;
                }
                f(entry);
            }
            sector = u32_at(data, 4 * (per_sector - 1))?;
        }
        Some(())
    }

    /// How many leading DIFAT entries cfb keeps after it drops the padding at
    /// the end: trailing zeros past the header and past `num_fat_sectors`
    /// (some writers pad with zeros), then trailing free entries.
    fn kept_entries(&self) -> Option<usize> {
        let mut len = 0usize;
        let mut last_nonzero = None;
        let mut last_used = None;
        let mut last_used_at_last_nonzero = None;
        self.for_each_entry(|entry| {
            if entry != FREE_SECTOR {
                last_used = Some(len);
            }
            if entry != 0 {
                last_nonzero = Some(len);
                last_used_at_last_nonzero = last_used;
            }
            len += 1;
        })?;
        let floor = HEADER_DIFAT_ENTRIES.max(self.num_fat_sectors as usize);
        let after_nonzero = last_nonzero.map_or(0, |p| p + 1);
        let without_zeros = if len <= floor {
            len
        } else {
            floor.max(after_nonzero)
        };
        // Ends in a zero that stayed: no free entry to drop.
        if without_zeros > after_nonzero {
            return Some(without_zeros);
        }
        Some(last_used_at_last_nonzero.map_or(0, |p| p + 1))
    }
}

/// Rejects a compound file whose DIFAT lists a FAT sector twice. Files cfb
/// rejects anyway pass.
pub(super) fn check_fat_sectors(bytes: &[u8]) -> Result<(), DomainError> {
    let Some(layout) = Layout::parse(bytes) else {
        return Ok(());
    };
    let Some(kept) = layout.kept_entries() else {
        return Ok(());
    };
    let mut seen = vec![false; layout.num_sectors as usize];
    let mut index = 0usize;
    let mut done = false;
    let mut problem = None;
    layout.for_each_entry(|entry| {
        if done || index >= kept {
            done = true;
            return;
        }
        index += 1;
        // cfb fails at the first index outside the file and reads no further.
        let Some(slot) = seen.get_mut(entry as usize) else {
            done = true;
            return;
        };
        if *slot {
            problem = Some(format!("FAT sector {entry} is listed twice"));
            done = true;
        }
        *slot = true;
    });
    match problem {
        Some(detail) => Err(DomainError::parse_error(format!(
            "Failed to parse XLS: invalid compound file: {detail}"
        ))),
        None => Ok(()),
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(super) mod tests {
    use super::*;

    /// A version 3 compound file of `sectors` 512-byte sectors after the
    /// header, whose header lists `header_fat` and whose DIFAT chain runs
    /// through sectors 1..=`difat_sectors`, each listing `fill` 127 times.
    pub(in super::super) fn crafted(
        sectors: usize,
        header_fat: &[u32],
        difat_sectors: u32,
        fill: u32,
    ) -> Vec<u8> {
        let mut out = vec![0u8; HEADER_LEN * (sectors + 1)];
        out[..8].copy_from_slice(&MAGIC);
        out[24..26].copy_from_slice(&0x3E_u16.to_le_bytes());
        out[26..28].copy_from_slice(&3_u16.to_le_bytes());
        out[28..30].copy_from_slice(&0xFFFE_u16.to_le_bytes());
        out[30..32].copy_from_slice(&9_u16.to_le_bytes());
        out[32..34].copy_from_slice(&6_u16.to_le_bytes());
        let fat_count = u32::try_from(header_fat.len()).expect("few") + difat_sectors * 127;
        out[44..48].copy_from_slice(&fat_count.to_le_bytes());
        out[48..52].copy_from_slice(&END_OF_CHAIN.to_le_bytes());
        out[56..60].copy_from_slice(&4096_u32.to_le_bytes());
        out[60..64].copy_from_slice(&END_OF_CHAIN.to_le_bytes());
        let first = if difat_sectors == 0 { END_OF_CHAIN } else { 1 };
        out[68..72].copy_from_slice(&first.to_le_bytes());
        out[72..76].copy_from_slice(&difat_sectors.to_le_bytes());
        for i in 0..HEADER_DIFAT_ENTRIES {
            let entry = header_fat.get(i).copied().unwrap_or(FREE_SECTOR);
            let at = HEADER_DIFAT_OFFSET + 4 * i;
            out[at..at + 4].copy_from_slice(&entry.to_le_bytes());
        }
        for k in 1..=difat_sectors {
            let start = (k as usize + 1) * HEADER_LEN;
            for i in 0..127 {
                out[start + 4 * i..start + 4 * i + 4].copy_from_slice(&fill.to_le_bytes());
            }
            let next = if k == difat_sectors {
                END_OF_CHAIN
            } else {
                k + 1
            };
            out[start + 508..start + 512].copy_from_slice(&next.to_le_bytes());
        }
        out
    }

    #[test]
    fn a_fat_sector_listed_many_times_is_rejected() {
        // Every DIFAT entry names sector 70: cfb would read 8237 sectors
        // (4.2 MB) of FAT from a 37 KB file, about 113 times its size.
        let bytes = crafted(72, &[70; HEADER_DIFAT_ENTRIES], 64, 70);
        let err = check_fat_sectors(&bytes).unwrap_err().to_string();
        assert!(err.contains("FAT sector 70 is listed twice"), "{err}");
    }

    #[test]
    fn spare_free_fat_sectors_are_accepted() {
        // 10 sectors need one FAT sector; cfb drops the free entries past
        // the file end that sectors 1 and 2 add, so three distinct ones pass.
        let mut bytes = crafted(10, &[0, 1, 2], 0, 0);
        bytes[2 * HEADER_LEN..4 * HEADER_LEN].fill(0xFF);
        check_fat_sectors(&bytes).unwrap();
    }

    #[test]
    fn zero_padding_after_the_last_fat_sector_is_accepted() {
        // Sector 0 in the header, DIFAT sector 1 padded with zeros: cfb drops
        // trailing zeros past num_fat_sectors, so nothing is read twice.
        let mut bytes = crafted(4, &[0], 1, 0);
        bytes[44..48].copy_from_slice(&1_u32.to_le_bytes());
        check_fat_sectors(&bytes).unwrap();
    }

    #[test]
    fn zero_padding_in_a_difat_sector_is_trimmed_like_cfb() {
        // The header lists 109 distinct sectors including 0; DIFAT sector 1
        // lists sector 110 and then zeros. cfb drops zeros only past
        // num_fat_sectors, so with 200 it keeps 90 of them and reads
        // sector 0 again.
        let header: Vec<u32> = std::iter::once(0).chain(2..=109).collect();
        let mut bytes = crafted(120, &header, 1, 0);
        bytes[2 * HEADER_LEN..2 * HEADER_LEN + 4].copy_from_slice(&110_u32.to_le_bytes());
        bytes[44..48].copy_from_slice(&110_u32.to_le_bytes());
        check_fat_sectors(&bytes).unwrap();
        bytes[44..48].copy_from_slice(&200_u32.to_le_bytes());
        let err = check_fat_sectors(&bytes).unwrap_err().to_string();
        assert!(err.contains("FAT sector 0 is listed twice"), "{err}");
    }

    #[test]
    fn real_compound_files_pass() {
        // The larger one needs more FAT sectors than the header holds, so
        // its DIFAT continues in a DIFAT sector.
        for len in [300_000, 8 * 1024 * 1024] {
            let file = super::super::xls::biff::xls(&vec![7u8; len]);
            check_fat_sectors(&file).unwrap();
        }
    }

    #[test]
    fn non_compound_input_is_left_to_cfb() {
        check_fat_sectors(b"not a compound file").unwrap();
        let mut bytes = crafted(4, &[0], 0, 0);
        bytes[30] = 7; // bad sector shift
        check_fat_sectors(&bytes).unwrap();
    }
}
