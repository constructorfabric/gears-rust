//! FX & multi-currency domain (Slice 5): pure functional-currency translation
//! (`translate`), pure realized-FX gain/loss on a cross-currency close
//! (`realized`), pure period-end unrealized revaluation (`revaluation`), and the
//! per-tenant revaluation mode (`revaluation_mode`, VHP-1986).

pub mod realized;
pub mod revaluation;
pub mod revaluation_mode;
pub mod translate;
