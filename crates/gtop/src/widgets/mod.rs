//! Monitor-specific widgets that aren't part of the generic toolkit.
//!
//! [`data_table::DataTable`] is the process table — sortable, with group
//! headers and a tree mode. It reads `MonitorTheme` directly for the CPU /
//! MEM gradients, which is why it lives here rather than in `gtui`.
//! The planned generic `LiveTable<R, G>` will eventually subsume it.

pub mod data_table;

pub use data_table::{
    DataTable, TableGroupHeader, TableLayout, TableRow, TableRowMeta, TableSort,
};
