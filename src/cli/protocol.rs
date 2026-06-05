use serde::{Deserialize, Serialize};

use crate::{
    CONVERGENCE_REPORT_SCHEMA, ENVIRONMENT_SCHEMA, LOCK_SCHEMA, PROGRESS_SCHEMA,
    TRACE_REPORT_SCHEMA, TRACE_SCHEMA,
};

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Capabilities {
    pub(crate) schema: String,
    pub(crate) version: String,
    pub(crate) lock_schema: String,
    pub(crate) environment_schema: String,
    pub(crate) trace_schema: String,
    pub(crate) trace_report_schema: String,
    pub(crate) convergence_report_schema: String,
    pub(crate) progress_schema: String,
}

impl Capabilities {
    pub(crate) fn current() -> Self {
        Self {
            schema: "pqty.capabilities/v1".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            lock_schema: LOCK_SCHEMA.to_string(),
            environment_schema: ENVIRONMENT_SCHEMA.to_string(),
            trace_schema: TRACE_SCHEMA.to_string(),
            trace_report_schema: TRACE_REPORT_SCHEMA.to_string(),
            convergence_report_schema: CONVERGENCE_REPORT_SCHEMA.to_string(),
            progress_schema: PROGRESS_SCHEMA.to_string(),
        }
    }
}
