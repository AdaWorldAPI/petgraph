//! The second consumer of the same prepared layout: a Quack-compiled one-hop.
//!
//! `src == s` lowers to one mask-risc program over the `src` lane; the kept
//! rows' neighbours are then read from the `dst` lane at those same rows.
//! Both lanes are borrowed from the [`PreparedRelation`] the petgraph view
//! borrows, so the adjacency is prepared once and read twice. The `dst` read
//! is at the kept rows' own coordinates, not at positions taken from another
//! lane's values, so it is not a gather.

use lance_graph_mask_risc::{
    ExecError, Foreign, LaneRef, Out, Planes, Scratch, execute_into, words_for,
};
use lance_graph_quack::{Agg, Cmp, Col, Filter, LowerError, Query, lower};

use crate::{PreparedRelation, Refusal, RelationVersion};

#[derive(Debug)]
pub enum HopError {
    Refused(Refusal),
    Lower(LowerError),
    Exec(ExecError),
}

/// What one compiled hop read, so a test can prove which buffers it touched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HopReport {
    pub version: RelationVersion,
    pub src_ptr: *const u32,
    pub dst_ptr: *const u32,
    pub kept_rows: usize,
}

/// The out-neighbours of `s`, in row order, appended to `out`.
pub fn one_hop(
    rel: &PreparedRelation,
    asked: RelationVersion,
    s: u32,
    out: &mut Vec<u32>,
) -> Result<HopReport, HopError> {
    if asked != rel.version() {
        return Err(HopError::Refused(Refusal::Stale {
            prepared: rel.version(),
            asked,
        }));
    }
    let n = rel.n_edges();
    let program = lower(&Query {
        filter: Filter::cmp(Col(0), Cmp::EqU32(s)),
        agg: Agg::Rows,
    })
    .map_err(HopError::Lower)?;
    let lanes = [LaneRef::U32(rel.src_lane())];
    let planes = Planes {
        n_rows: n,
        masks: &[],
        lanes: &lanes,
    };
    let mut scratch = Scratch::for_program(&program, n).map_err(HopError::Exec)?;
    let mut mask = vec![0u64; words_for(n)];
    execute_into(
        &program,
        &planes,
        &Foreign::NONE,
        &mut scratch,
        Out::Mask(&mut mask),
    )
    .map_err(HopError::Exec)?;
    let dst = rel.dst_lane();
    let mut kept = 0;
    for (w, &word) in mask.iter().enumerate() {
        let mut bits = word;
        while bits != 0 {
            let row = w * 64 + bits.trailing_zeros() as usize;
            bits &= bits - 1;
            if row < n {
                out.push(dst[row]);
                kept += 1;
            }
        }
    }
    Ok(HopReport {
        version: rel.version(),
        src_ptr: rel.src_lane().as_ptr(),
        dst_ptr: dst.as_ptr(),
        kept_rows: kept,
    })
}
