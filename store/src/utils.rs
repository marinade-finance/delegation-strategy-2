use postgres::types::ToSql;
use postgres::{Client, NoTls};

pub struct InsertQueryCombiner<'a> {
    pub insertions: u64,
    statement: String,
    params: Vec<&'a (dyn ToSql + Sync)>,
}

impl<'a> InsertQueryCombiner<'a> {
    pub fn new(table_name: String, columns: String) -> Self {
        Self {
            insertions: 0,
            statement: format!("INSERT INTO {} ({}) VALUES", table_name, columns).to_string(),
            params: vec![],
        }
    }

    pub fn add(&mut self, values: &mut Vec<&'a (dyn ToSql + Sync)>) {
        let separator = if self.insertions == 0 { " " } else { "," };
        let mut query_end = "(".to_string();
        for i in 0..values.len() {
            if i > 0 {
                query_end.push_str(",");
            }
            query_end.push_str(&format!("${}", i + 1 + self.params.len()));
        }
        query_end.push_str(")");

        self.params.append(values);
        self.statement
            .push_str(&format!("{}{}", separator, query_end));
        self.insertions += 1;
    }

    pub fn execute(&self, client: &mut Client) -> anyhow::Result<Option<u64>> {
        if self.insertions == 0 {
            return Ok(None);
        }

        Ok(Some(client.execute(&self.statement, &self.params)?))
    }
}

pub struct UpdateQueryCombiner<'a> {
    pub updates: u64,
    statement: String,
    values_names: String,
    where_condition: String,
    params: Vec<&'a (dyn ToSql + Sync)>,
}

impl<'a> UpdateQueryCombiner<'a> {
    pub fn new(
        table_name: String,
        updates: String,
        values_names: String,
        where_condition: String,
    ) -> Self {
        Self {
            updates: 0,
            statement: format!("UPDATE {} SET {} FROM (VALUES", table_name, updates).to_string(),
            values_names,
            where_condition,
            params: vec![],
        }
    }

    pub fn add(&mut self, values: &mut Vec<&'a (dyn ToSql + Sync)>) {
        let separator = if self.updates == 0 { " " } else { "," };
        let mut query_end = "(".to_string();
        for i in 0..values.len() {
            if i > 0 {
                query_end.push_str(",");
            }
            query_end.push_str(&format!("${}", i + 1 + self.params.len()));
            if i == 0 {
                query_end.push_str("::bigint");
            }
            if i == 1 {
                query_end.push_str("::timestamp with time zone");
            }
        }
        query_end.push_str(")");

        self.params.append(values);
        self.statement
            .push_str(&format!("{}{}", separator, query_end));
        self.updates += 1;
    }

    pub fn execute(&mut self, client: &mut Client) -> anyhow::Result<Option<u64>> {
        if self.updates == 0 {
            return Ok(None);
        }

        self.statement.push_str(&format!(
            ") AS {} WHERE {}",
            self.values_names, self.where_condition
        ));

        Ok(Some(client.execute(&self.statement, &self.params)?))
    }
}
