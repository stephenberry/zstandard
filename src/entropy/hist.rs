use crate::{
    entropy::mem::read_u32,
    error::{Error, Result},
};

pub(crate) const HIST_WKSP_SIZE_U32: usize = 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
enum CheckInput {
    TrustInput,
    CheckMaxSymbolValue,
}

#[inline]
pub(crate) fn count_simple(count: &mut [u32; 256], max_symbol_value: &mut u32, src: &[u8]) -> u32 {
    count[..=*max_symbol_value as usize].fill(0);
    if src.is_empty() {
        *max_symbol_value = 0;
        return 0;
    }

    for &byte in src {
        debug_assert!(u32::from(byte) <= *max_symbol_value);
        count[byte as usize] += 1;
    }

    while *max_symbol_value > 0 && count[*max_symbol_value as usize] == 0 {
        *max_symbol_value -= 1;
    }

    let mut largest = 0;
    for &value in &count[..=*max_symbol_value as usize] {
        largest = largest.max(value);
    }
    largest
}

#[allow(unsafe_code)]
fn count_parallel_wksp(
    count: &mut [u32; 256],
    max_symbol_value: &mut u32,
    source: &[u8],
    check: CheckInput,
    workspace: &mut [u32; HIST_WKSP_SIZE_U32],
) -> Result<u32> {
    debug_assert!(*max_symbol_value <= 255);
    if source.is_empty() {
        count.fill(0);
        *max_symbol_value = 0;
        return Ok(0);
    }

    workspace.fill(0);
    // Use raw pointers into the workspace to eliminate bounds checks in the
    // hot loop. Each index is `(value >> shift) & 0xFF` which is always in
    // 0..256, but LLVM can't prove this through the split_at_mut indirection
    // (3 panic paths survive in the emitted asm without this).
    let c1 = workspace.as_mut_ptr();
    let c2 = unsafe { c1.add(256) };
    let c3 = unsafe { c1.add(512) };
    let c4 = unsafe { c1.add(768) };

    macro_rules! inc4 {
        ($c1:expr, $c2:expr, $c3:expr, $c4:expr, $v:expr) => {
            unsafe {
                *$c1.add(($v & 0xFF) as usize) += 1;
                *$c2.add((($v >> 8) & 0xFF) as usize) += 1;
                *$c3.add((($v >> 16) & 0xFF) as usize) += 1;
                *$c4.add(($v >> 24) as usize) += 1;
            }
        };
    }

    let mut offset = 0usize;
    let len = source.len();
    if len >= 4 {
        let mut cached = read_u32(source, offset);
        offset += 4;
        while offset + 15 < len {
            let mut value = cached;
            cached = read_u32(source, offset);
            offset += 4;
            inc4!(c1, c2, c3, c4, value);

            value = cached;
            cached = read_u32(source, offset);
            offset += 4;
            inc4!(c1, c2, c3, c4, value);

            value = cached;
            cached = read_u32(source, offset);
            offset += 4;
            inc4!(c1, c2, c3, c4, value);

            value = cached;
            cached = read_u32(source, offset);
            offset += 4;
            inc4!(c1, c2, c3, c4, value);
        }
        offset -= 4;
    }

    for &byte in &source[offset..] {
        unsafe { *c1.add(byte as usize) += 1 };
    }

    // Merge the 4 counting arrays.
    let counting1 = unsafe { core::slice::from_raw_parts_mut(c1, 256) };
    let counting2 = unsafe { core::slice::from_raw_parts(c2, 256) };
    let counting3 = unsafe { core::slice::from_raw_parts(c3, 256) };
    let counting4 = unsafe { core::slice::from_raw_parts(c4, 256) };
    let mut max_count = 0;
    for index in 0..256 {
        counting1[index] += counting2[index] + counting3[index] + counting4[index];
        max_count = max_count.max(counting1[index]);
    }

    let mut max_symbol = 255usize;
    while counting1[max_symbol] == 0 {
        max_symbol -= 1;
    }
    if check == CheckInput::CheckMaxSymbolValue && max_symbol as u32 > *max_symbol_value {
        return Err(Error::MaxSymbolValueTooSmall);
    }

    *max_symbol_value = max_symbol as u32;
    count[..=max_symbol].copy_from_slice(&counting1[..=max_symbol]);
    Ok(max_count)
}

pub(crate) fn count_fast_wksp(
    count: &mut [u32; 256],
    max_symbol_value: &mut u32,
    source: &[u8],
    workspace: &mut [u32; HIST_WKSP_SIZE_U32],
) -> Result<u32> {
    if source.len() < 1500 {
        return Ok(count_simple(count, max_symbol_value, source));
    }

    count_parallel_wksp(
        count,
        max_symbol_value,
        source,
        CheckInput::TrustInput,
        workspace,
    )
}

pub(crate) fn count_wksp(
    count: &mut [u32; 256],
    max_symbol_value: &mut u32,
    source: &[u8],
    workspace: &mut [u32; HIST_WKSP_SIZE_U32],
) -> Result<u32> {
    if *max_symbol_value < 255 {
        return count_parallel_wksp(
            count,
            max_symbol_value,
            source,
            CheckInput::CheckMaxSymbolValue,
            workspace,
        );
    }

    *max_symbol_value = 255;
    count_fast_wksp(count, max_symbol_value, source, workspace)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(len: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(len);
        let mut state = 0x1234_5678u32;
        for index in 0..len {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let byte = match index & 7 {
                0..=4 => 0,
                5 => b'a',
                6 => b'b',
                _ => (state & 0x0F) as u8,
            };
            out.push(byte);
        }
        out
    }

    #[test]
    fn parallel_count_matches_simple() {
        let src = sample(128 * 1024);
        let mut fast = [0u32; 256];
        let mut simple = [0u32; 256];
        let mut fast_max = 255u32;
        let mut simple_max = 255u32;
        let mut workspace = [0u32; HIST_WKSP_SIZE_U32];
        let fast_largest = count_wksp(&mut fast, &mut fast_max, &src, &mut workspace).unwrap();
        let simple_largest = count_simple(&mut simple, &mut simple_max, &src);

        assert_eq!(fast_max, simple_max);
        assert_eq!(fast_largest, simple_largest);
        assert_eq!(fast, simple);
    }

    #[test]
    fn rejects_too_small_max_symbol_value() {
        let src = [0u8, 1, 2, 3, 4, 200];
        let mut count = [0u32; 256];
        let mut max_symbol = 16u32;
        let mut workspace = [0u32; HIST_WKSP_SIZE_U32];

        let err = count_wksp(&mut count, &mut max_symbol, &src, &mut workspace).unwrap_err();
        assert_eq!(err, Error::MaxSymbolValueTooSmall);
    }
}
