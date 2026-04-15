use std::fmt;

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum ColumnFamilyName {
    Default,
    Blocks,
    Headers,
    Transactions,
    Utxo,
    UtxoSpent,
    VerkleState,
    Dag,
    DagTips,
}

impl ColumnFamilyName {
    pub fn as_str(&self) -> &'static str {
        match self {
            ColumnFamilyName::Default => "default",
            ColumnFamilyName::Blocks => "blocks",
            ColumnFamilyName::Headers => "headers",
            ColumnFamilyName::Transactions => "transactions",
            ColumnFamilyName::Utxo => "utxo",
            ColumnFamilyName::UtxoSpent => "utxo_spent",
            ColumnFamilyName::VerkleState => "verkle_state",
            ColumnFamilyName::Dag => "dag",
            ColumnFamilyName::DagTips => "dag_tips",
        }
    }
}

impl fmt::Display for ColumnFamilyName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

pub fn all_column_families() -> &'static [ColumnFamilyName] {
    &[
        ColumnFamilyName::Default,
        ColumnFamilyName::Blocks,
        ColumnFamilyName::Headers,
        ColumnFamilyName::Transactions,
        ColumnFamilyName::Utxo,
        ColumnFamilyName::UtxoSpent,
        ColumnFamilyName::VerkleState,
        ColumnFamilyName::Dag,
        ColumnFamilyName::DagTips,
    ]
}
