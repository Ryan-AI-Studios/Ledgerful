use crate::ledger::enforcement::*;
use crate::ledger::error::LedgerError;
use rusqlite::{Connection, OptionalExtension, params};

pub fn insert_tech_stack_rule(conn: &Connection, rule: &TechStackRule) -> Result<(), LedgerError> {
    conn.execute(
        "INSERT INTO tech_stack (
            category, name, version_constraint, rules, locked, status, entity_type, registered_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
        ON CONFLICT(category) DO UPDATE SET
            name = EXCLUDED.name,
            version_constraint = EXCLUDED.version_constraint,
            rules = EXCLUDED.rules,
            locked = EXCLUDED.locked,
            status = EXCLUDED.status,
            entity_type = EXCLUDED.entity_type,
            registered_at = EXCLUDED.registered_at",
        params![
            rule.category,
            rule.name,
            rule.version_constraint,
            serde_json::to_string(&rule.rules).map_err(|e| LedgerError::Config(e.to_string()))?,
            rule.locked as i32,
            rule.status,
            rule.entity_type,
            rule.registered_at,
        ],
    )?;
    Ok(())
}

pub fn get_tech_stack_rules(
    conn: &Connection,
    category: Option<&str>,
) -> Result<Vec<TechStackRule>, LedgerError> {
    let mut sql = "SELECT category, name, version_constraint, rules, locked, status, entity_type, registered_at
         FROM tech_stack".to_string();

    let rules = if let Some(cat) = category {
        sql.push_str(" WHERE category = ?1");
        sql.push_str(" ORDER BY category ASC");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([cat], map_tech_stack_rule)?;
        rows.collect::<Result<Vec<_>, _>>()?
    } else {
        sql.push_str(" ORDER BY category ASC");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], map_tech_stack_rule)?;
        rows.collect::<Result<Vec<_>, _>>()?
    };

    Ok(rules)
}

pub fn get_tech_stack_rule(
    conn: &Connection,
    category: &str,
) -> Result<Option<TechStackRule>, LedgerError> {
    let mut stmt = conn.prepare(
        "SELECT category, name, version_constraint, rules, locked, status, entity_type, registered_at
         FROM tech_stack WHERE category = ?1",
    )?;

    stmt.query_row([category], map_tech_stack_rule)
        .optional()
        .map_err(LedgerError::from)
}

fn map_tech_stack_rule(row: &rusqlite::Row) -> rusqlite::Result<TechStackRule> {
    let rules_json: String = row.get(3)?;
    let rules: Vec<String> =
        serde_json::from_str(&rules_json).map_err(|_| rusqlite::Error::InvalidQuery)?;
    Ok(TechStackRule {
        category: row.get(0)?,
        name: row.get(1)?,
        version_constraint: row.get(2)?,
        rules,
        locked: row.get::<_, i32>(4)? != 0,
        status: row.get(5)?,
        entity_type: row.get(6)?,
        registered_at: row.get(7)?,
    })
}

pub fn insert_commit_validator(
    conn: &Connection,
    validator: &CommitValidator,
) -> Result<(), LedgerError> {
    conn.execute(
        "INSERT INTO commit_validators (
            category, name, description, executable, args, timeout_ms, glob, validation_level, enabled
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
        ON CONFLICT(name, category) DO UPDATE SET
            executable = EXCLUDED.executable,
            args = EXCLUDED.args,
            timeout_ms = EXCLUDED.timeout_ms,
            enabled = EXCLUDED.enabled",
        params![
            validator.category,
            validator.name,
            validator.description,
            validator.executable,
            serde_json::to_string(&validator.args)
                .map_err(|e| LedgerError::Config(e.to_string()))?,
            validator.timeout_ms,
            validator.glob,
            serde_json::to_string(&validator.validation_level)
                .map_err(|e| LedgerError::Config(e.to_string()))?
                .trim_matches('"'),
            validator.enabled as i32,
        ],
    )?;
    Ok(())
}

pub fn set_validator_enabled(
    conn: &Connection,
    name: &str,
    enabled: bool,
) -> Result<(), LedgerError> {
    conn.execute(
        "UPDATE commit_validators SET enabled = ?1 WHERE name = ?2",
        rusqlite::params![enabled as i32, name],
    )?;
    Ok(())
}

pub fn remove_validator(conn: &Connection, name: &str) -> Result<(), LedgerError> {
    conn.execute("DELETE FROM commit_validators WHERE name = ?1", [name])?;
    Ok(())
}

pub fn get_commit_validators(
    conn: &Connection,
    category: Option<&str>,
) -> Result<Vec<CommitValidator>, LedgerError> {
    let mut sql = "SELECT id, category, name, description, executable, args, timeout_ms, glob, validation_level, enabled
         FROM commit_validators".to_string();

    let validators = if let Some(cat) = category {
        sql.push_str(" WHERE (category = ?1 OR category = 'ALL')");
        sql.push_str(" ORDER BY category ASC");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([cat], map_commit_validator)?;
        rows.collect::<Result<Vec<_>, _>>()?
    } else {
        sql.push_str(" ORDER BY category ASC");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], map_commit_validator)?;
        rows.collect::<Result<Vec<_>, _>>()?
    };

    Ok(validators)
}

fn map_commit_validator(row: &rusqlite::Row) -> rusqlite::Result<CommitValidator> {
    let args_json: String = row.get(5)?;
    let args: Vec<String> =
        serde_json::from_str(&args_json).map_err(|_| rusqlite::Error::InvalidQuery)?;
    let vl_str: String = row.get(8)?;
    let validation_level: ValidationLevel = serde_json::from_str(&format!("\"{}\"", vl_str))
        .map_err(|_| rusqlite::Error::InvalidQuery)?;
    Ok(CommitValidator {
        id: Some(row.get(0)?),
        category: row.get(1)?,
        name: row.get(2)?,
        description: row.get(3)?,
        executable: row.get(4)?,
        args,
        timeout_ms: row.get(6)?,
        glob: row.get(7)?,
        validation_level,
        enabled: row.get::<_, i32>(9)? != 0,
    })
}

pub fn insert_category_mapping(
    conn: &Connection,
    mapping: &CategoryStackMapping,
) -> Result<(), LedgerError> {
    conn.execute(
        "INSERT INTO category_stack_mappings (
            ledger_category, stack_category, glob, description
        ) VALUES (?1, ?2, ?3, ?4)",
        params![
            mapping.ledger_category,
            mapping.stack_category,
            mapping.glob,
            mapping.description,
        ],
    )?;
    Ok(())
}

pub fn get_category_mappings(
    conn: &Connection,
    category: Option<&str>,
) -> Result<Vec<CategoryStackMapping>, LedgerError> {
    let mut sql = "SELECT id, ledger_category, stack_category, glob, description
         FROM category_stack_mappings"
        .to_string();

    let mappings = if let Some(cat) = category {
        sql.push_str(" WHERE ledger_category = ?1 OR stack_category = ?1");
        sql.push_str(" ORDER BY ledger_category ASC");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([cat], |row| {
            Ok(CategoryStackMapping {
                id: Some(row.get(0)?),
                ledger_category: row.get(1)?,
                stack_category: row.get(2)?,
                glob: row.get(3)?,
                description: row.get(4)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
    } else {
        sql.push_str(" ORDER BY ledger_category ASC");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| {
            Ok(CategoryStackMapping {
                id: Some(row.get(0)?),
                ledger_category: row.get(1)?,
                stack_category: row.get(2)?,
                glob: row.get(3)?,
                description: row.get(4)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
    };

    Ok(mappings)
}

pub fn insert_watcher_pattern(
    conn: &Connection,
    pattern: &WatcherPattern,
) -> Result<(), LedgerError> {
    conn.execute(
        "INSERT INTO watcher_patterns (
            glob, category, source, description
        ) VALUES (?1, ?2, ?3, ?4)",
        params![
            pattern.glob,
            pattern.category,
            pattern.source,
            pattern.description,
        ],
    )?;
    Ok(())
}

pub fn get_watcher_patterns(conn: &Connection) -> Result<Vec<WatcherPattern>, LedgerError> {
    let mut stmt = conn.prepare(
        "SELECT id, glob, category, source, description
         FROM watcher_patterns ORDER BY glob ASC",
    )?;

    let rows = stmt.query_map([], |row| {
        Ok(WatcherPattern {
            id: Some(row.get(0)?),
            glob: row.get(1)?,
            category: row.get(2)?,
            source: row.get(3)?,
            description: row.get(4)?,
        })
    })?;

    let mut patterns = Vec::new();
    for p in rows {
        patterns.push(p?);
    }
    Ok(patterns)
}

pub fn register_forbidden_term(
    conn: &Connection,
    term: &str,
    category: &str,
    _reason: &str,
) -> Result<(), LedgerError> {
    let rule = TechStackRule {
        category: category.to_string(),
        name: term.to_string(),
        version_constraint: Some("*".to_string()),
        rules: vec![format!("NO {}", term)],
        locked: true,
        status: "FORBIDDEN".to_string(),
        entity_type: "TERM".to_string(),
        registered_at: chrono::Utc::now().to_rfc3339(),
    };
    insert_tech_stack_rule(conn, &rule)
}

pub fn register_validator(
    conn: &Connection,
    name: &str,
    command: &str,
    category: &str,
    timeout_secs: u64,
) -> Result<(), LedgerError> {
    let validator = CommitValidator {
        id: None,
        category: category.to_string(),
        name: name.to_string(),
        description: Some(format!("Manual validator: {}", name)),
        executable: command.to_string(),
        args: vec![],
        timeout_ms: (timeout_secs * 1000) as i32,
        glob: Some("*".to_string()),
        validation_level: ValidationLevel::Error,
        enabled: true,
    };
    insert_commit_validator(conn, &validator)
}
