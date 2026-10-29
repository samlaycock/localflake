use base64::Engine as _;

use super::*;

fn session() -> Session {
    let engine = Engine::open(None).unwrap();
    engine
        .session(SessionOptions { user: "TESTER".into(), database: Some("TEST".into()), ..Default::default() })
        .unwrap()
}

fn run(s: &mut Session, sql: &str) -> Arc<QueryResult> {
    match s.execute(&Request { sql, ..Default::default() }) {
        Ok(mut results) => results.remove(0),
        Err(failure) => panic!("{sql}: {}", failure.error),
    }
}

fn fail(s: &mut Session, sql: &str) -> EngineError {
    s.execute(&Request { sql, ..Default::default() }).err().unwrap_or_else(|| panic!("{sql} should fail")).error
}

fn rows(s: &mut Session, sql: &str) -> Vec<Vec<Option<String>>> {
    run(s, sql).json_rows().unwrap()
}

/// The single value of a one-row, one-column query, in JSON wire encoding.
fn value(s: &mut Session, sql: &str) -> Option<String> {
    let mut rows = rows(s, sql);
    assert_eq!(rows.len(), 1, "{sql}");
    rows.remove(0).remove(0)
}

fn v(s: &str) -> Option<String> {
    Some(s.to_owned())
}

#[test]
fn select_literal() {
    let mut s = session();
    let result = run(&mut s, "select 1");
    assert_eq!(result.columns[0].name, "1");
    assert_eq!(result.columns[0].sf_type, SfType::Fixed);
    assert_eq!(result.json_rows().unwrap(), vec![vec![v("1")]]);
}

#[test]
fn identifiers_and_column_names() {
    let mut s = session();
    run(&mut s, "create table t (id int, name varchar)");
    run(&mut s, "insert into t values (1, 'a'), (2, null)");
    let result = run(&mut s, "select id, name, id + 1, count(*) over () from T order by id");
    let names: Vec<_> = result.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["ID", "NAME", "ID + 1", "COUNT(*) OVER ()"]);
    run(&mut s, r#"create table "Mixed" ("lower" int)"#);
    assert_eq!(run(&mut s, r#"select "lower" from "Mixed""#).columns[0].name, "lower");
}

#[test]
fn dml_row_counts() {
    let mut s = session();
    run(&mut s, "create table t (a int)");
    assert_eq!(rows(&mut s, "insert into t values (1), (2), (3)"), vec![vec![v("3")]]);
    assert_eq!(rows(&mut s, "update t set a = a + 1 where a > 1"), vec![vec![v("2"), v("0")]]);
    assert_eq!(rows(&mut s, "delete from t where a = 1"), vec![vec![v("1")]]);
    assert_eq!(rows(&mut s, "insert overwrite into t select 9"), vec![vec![v("1")]]);
    assert_eq!(value(&mut s, "select count(*) from t"), v("1"));
}

#[test]
fn nulls_sort_high() {
    let mut s = session();
    run(&mut s, "create table t (v int)");
    run(&mut s, "insert into t values (2), (null), (1)");
    let col = |r: Vec<Vec<Option<String>>>| r.into_iter().map(|mut r| r.remove(0)).collect::<Vec<_>>();
    assert_eq!(col(rows(&mut s, "select v from t order by v")), [v("1"), v("2"), None]);
    assert_eq!(col(rows(&mut s, "select v from t order by v desc")), [None, v("2"), v("1")]);
}

#[test]
fn type_mapping() {
    let mut s = session();
    run(
        &mut s,
        "create table t (a number(38,0), b number(10,2), c string, d timestamp_ntz, e variant, f float, g boolean, h date)",
    );
    run(
        &mut s,
        "insert into t select 1, 2.5, 'x', '2024-01-02 03:04:05.123456', parse_json('{\"k\": [1, 2]}'), 1.5, true, '2024-01-02'",
    );
    let result = run(&mut s, "select * from t");
    let types: Vec<_> = result.columns.iter().map(|c| c.sf_type).collect();
    use SfType::*;
    assert_eq!(types, [Fixed, Fixed, Text, TimestampNtz, Variant, Real, Boolean, Date]);
    assert_eq!((result.columns[1].precision, result.columns[1].scale), (Some(10), Some(2)));
    assert_eq!(
        result.json_rows().unwrap(),
        vec![vec![
            v("1"),
            v("2.50"),
            v("x"),
            v("1704164645.123456000"),
            v("{\n  \"k\": [\n    1,\n    2\n  ]\n}"),
            v("1.5"),
            v("1"),
            v("19724"),
        ]]
    );
}

#[test]
fn integer_division_and_division_by_zero() {
    let mut s = session();
    assert_eq!(value(&mut s, "select 1 / 3"), v("0.333333"));
    assert_eq!(value(&mut s, "select 2 / 3"), v("0.666667"));
    assert_eq!(value(&mut s, "select -2 / 3"), v("-0.666667"));
    assert_eq!(value(&mut s, "select 1.5::float / 2"), v("0.75"));
    let err = fail(&mut s, "select 1 / 0");
    assert_eq!((err.code, err.message.as_str()), ("100051", "Division by zero"));
    assert_eq!(value(&mut s, "select div0(1, 0)"), v("0.000000"));
}

#[test]
fn conditional_functions() {
    let mut s = session();
    assert_eq!(value(&mut s, "select iff(1 > 2, 'a', 'b')"), v("b"));
    assert_eq!(value(&mut s, "select nvl(null, 'x')"), v("x"));
    assert_eq!(value(&mut s, "select nvl2(null, 'a', 'b')"), v("b"));
    assert_eq!(value(&mut s, "select zeroifnull(null)"), v("0"));
    assert_eq!(value(&mut s, "select decode(2, 1, 'one', 2, 'two', 'other')"), v("two"));
    assert_eq!(value(&mut s, "select decode(null, null, 'null matched', 'no')"), v("null matched"));
    assert_eq!(value(&mut s, "select greatest(1, null, 3)"), None);
    assert_eq!(value(&mut s, "select concat('a', null)"), None);
    assert_eq!(value(&mut s, "select concat('a', 'b', 1)"), v("ab1"));
    assert_eq!(value(&mut s, "select equal_null(null, null)"), v("1"));
}

#[test]
fn string_functions() {
    let mut s = session();
    assert_eq!(value(&mut s, "select len('abc')"), v("3"));
    assert_eq!(value(&mut s, "select charindex('b', 'abcb')"), v("2"));
    assert_eq!(value(&mut s, "select charindex('b', 'abcb', 3)"), v("4"));
    assert_eq!(value(&mut s, "select startswith('abc', 'ab')"), v("1"));
    assert_eq!(value(&mut s, "select split_part('a,b,c', ',', 2)"), v("b"));
    assert_eq!(value(&mut s, "select strtok('a,,b', ',', 2)"), v("b"));
    assert_eq!(value(&mut s, "select regexp_replace('aaa', 'a', 'b')"), v("bbb"));
    assert_eq!(value(&mut s, "select regexp_like('abc', 'b')"), v("0"));
    assert_eq!(value(&mut s, "select regexp_like('abc', '.*b.*')"), v("1"));
    assert_eq!(value(&mut s, "select regexp_substr('abc123', '[0-9]+')"), v("123"));
    assert_eq!(value(&mut s, "select regexp_substr('abc', '[0-9]+')"), None);
    assert_eq!(value(&mut s, "select initcap('hello world')"), v("Hello World"));
    assert_eq!(value(&mut s, "select 'abc' like any ('x%', 'a%')"), v("1"));
    assert_eq!(value(&mut s, "select 'a\\nb'"), v("a\nb"));
    assert_eq!(value(&mut s, "select $$it's$$"), v("it's"));
    assert_eq!(
        value(&mut s, "select listagg(x, ',') within group (order by x desc) from (values ('a'), ('b')) t(x)"),
        v("b,a")
    );
}

#[test]
fn date_functions() {
    let mut s = session();
    let days = |date: &str| {
        (chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").unwrap()
            - chrono::NaiveDate::from_ymd_opt(1970, 1, 1).unwrap())
        .num_days()
        .to_string()
    };
    assert_eq!(value(&mut s, "select dateadd(month, 1, '2024-01-31'::date)"), Some(days("2024-02-29")));
    assert_eq!(run(&mut s, "select dateadd(month, 1, '2024-01-31'::date)").columns[0].sf_type, SfType::Date);
    assert_eq!(run(&mut s, "select dateadd(hour, 1, '2024-01-31'::date)").columns[0].sf_type, SfType::TimestampNtz);
    assert_eq!(value(&mut s, "select dateadd(day, -1, '2024-03-01'::date)"), Some(days("2024-02-29")));
    assert_eq!(value(&mut s, "select datediff(day, '2024-01-01', '2024-03-01')"), v("60"));
    assert_eq!(value(&mut s, "select datediff('month', '2024-01-31'::date, '2024-02-01'::date)"), v("1"));
    assert_eq!(value(&mut s, "select date_trunc('month', '2024-03-15'::date)"), Some(days("2024-03-01")));
    assert_eq!(run(&mut s, "select date_trunc('month', '2024-03-15'::date)").columns[0].sf_type, SfType::Date);
    assert_eq!(value(&mut s, "select to_date('2024-03-15')"), Some(days("2024-03-15")));
    assert_eq!(value(&mut s, "select to_date('15/03/2024', 'DD/MM/YYYY')"), Some(days("2024-03-15")));
    assert_eq!(value(&mut s, "select to_char('2024-03-15'::date, 'YYYY/MM/DD')"), v("2024/03/15"));
    assert_eq!(value(&mut s, "select dayname('2024-03-15'::date)"), v("Fri"));
    assert_eq!(value(&mut s, "select year('2024-03-15'::date)"), v("2024"));
    assert_eq!(value(&mut s, "select last_day('2024-02-10'::date)"), Some(days("2024-02-29")));
    assert_eq!(value(&mut s, "select to_timestamp_ntz('2024-01-02T03:04:05Z')"), v("1704164645.000000000"));
    assert_eq!(value(&mut s, "select to_timestamp(1704164645)"), v("1704164645.000000000"));
    assert_eq!(value(&mut s, "select '1704164645'::timestamp"), v("1704164645.000000000"));
    assert_eq!(value(&mut s, "select try_to_date('nope')"), None);
    assert_eq!(fail(&mut s, "select to_date('nope')").code, "100040");
}

#[test]
fn semi_structured() {
    let mut s = session();
    run(&mut s, "create table t (v variant)");
    run(
        &mut s,
        r#"insert into t select parse_json('{"name": "ada", "Tags": ["x", "y"], "n": 1.5, "nested": {"k": 2}}')"#,
    );
    assert_eq!(value(&mut s, "select v:name from t"), v("\"ada\""));
    assert_eq!(value(&mut s, "select v:name::string from t"), v("ada"));
    assert_eq!(value(&mut s, "select v:Tags[1]::string from t"), v("y"));
    assert_eq!(value(&mut s, "select v:tags from t"), None, "path keys are case-sensitive");
    assert_eq!(value(&mut s, "select v:nested.k::int + 1 from t"), v("3"));
    assert_eq!(value(&mut s, "select v['nested']['k'] from t"), v("2"));
    assert_eq!(value(&mut s, "select get_path(v, 'Tags[0]')::string from t"), v("x"));
    assert_eq!(value(&mut s, "select v:missing from t"), None);
    assert_eq!(value(&mut s, "select v:name = 'ada' from t"), v("1"));
    assert_eq!(value(&mut s, "select array_size(v:Tags) from t"), v("2"));
    assert_eq!(value(&mut s, "select typeof(v:n) from t"), v("DOUBLE"));
    assert_eq!(value(&mut s, "select typeof(v:Tags) from t"), v("ARRAY"));
    assert_eq!(value(&mut s, "select v:nested::string from t"), v(r#"{"k":2}"#));
    assert_eq!(value(&mut s, "select object_construct('a', 1, 'b', null)"), v("{\n  \"a\": 1\n}"));
    assert_eq!(value(&mut s, "select array_construct(1, 'x')"), v("[\n  1,\n  \"x\"\n]"));
    assert_eq!(
        value(&mut s, "select array_agg(x) within group (order by x) from (values (2), (1), (null)) t(x)"),
        v("[\n  1,\n  2\n]")
    );
    assert_eq!(value(&mut s, "select array_contains('y'::variant, v:Tags) from t"), v("1"));
    assert_eq!(value(&mut s, "select array_to_string(v:Tags, '|') from t"), v("x|y"));
    assert_eq!(
        rows(&mut s, "select f.index, f.value::string from t, lateral flatten(input => v:Tags) f"),
        vec![vec![v("0"), v("x")], vec![v("1"), v("y")]]
    );
    assert_eq!(rows(&mut s, "select f.key, f.path from t, lateral flatten(v:nested) f"), vec![vec![v("k"), v("k")]]);
    assert_eq!(value(&mut s, "select count(*) from table(flatten(input => parse_json('[1,2,3]')))"), v("3"));
    assert_eq!(value(&mut s, "select count(*) from t, lateral flatten(input => v:none, outer => true) f"), v("1"));
}

#[test]
fn databases_schemas_and_use() {
    let mut s = session();
    run(&mut s, "create database other");
    assert_eq!((s.ctx.database.as_deref(), s.ctx.schema.as_deref()), (Some("OTHER"), Some("PUBLIC")));
    run(&mut s, "create table t (a int)");
    run(&mut s, "create schema staging");
    assert_eq!(s.ctx.schema.as_deref(), Some("STAGING"));
    run(&mut s, "use database test");
    assert_eq!(value(&mut s, "select count(*) from other.public.t"), v("0"));
    assert_eq!(value(&mut s, "select current_database()"), v("TEST"));
    assert_eq!(value(&mut s, "select current_schema()"), v("PUBLIC"));
    assert_eq!(fail(&mut s, "create database other").code, "002002");
    assert_eq!(fail(&mut s, "use schema nope").code, "002003");
    run(&mut s, "create database cloned clone other");
    assert_eq!(value(&mut s, "select count(*) from cloned.public.t"), v("0"));
    run(&mut s, "drop database other");
    assert_eq!(fail(&mut s, "use database other").code, "002003");
}

#[test]
fn ddl_features() {
    let mut s = session();
    run(&mut s, "create table t (id int autoincrement start 10 increment 5, a varchar primary key, b int unique)");
    run(&mut s, "insert into t (a) values ('x'), ('x')");
    assert_eq!(rows(&mut s, "select id from t order by id"), vec![vec![v("10")], vec![v("15")]]);
    assert_eq!(fail(&mut s, "insert into t (a) values (null)").code, "100072");
    run(&mut s, "create or replace table t2 as select a, count(*) as n from t group by a");
    assert_eq!(rows(&mut s, "select a, n from t2"), vec![vec![v("x"), v("2")]]);
    run(&mut s, "create table t3 clone t2");
    run(&mut s, "create table t4 like t2");
    assert_eq!(value(&mut s, "select count(*) from t4"), v("0"));
    run(&mut s, "alter table t2 add column c number(10,2)");
    run(&mut s, "alter table t3 swap with t4");
    assert_eq!(value(&mut s, "select count(*) from t3"), v("0"));
    run(&mut s, "create view v as select a from t2");
    assert_eq!(value(&mut s, "select a from v"), v("x"));
    assert_eq!(rows(&mut s, "drop table t4"), vec![vec![v("T4 successfully dropped.")]]);
    run(&mut s, "create transient table tt (a int) cluster by (a) data_retention_time_in_days = 0 comment = 'x'");
    run(&mut s, "truncate table tt");
}

#[test]
fn show_and_describe() {
    let mut s = session();
    run(&mut s, "create table people (id int not null, name varchar, score number(10,2))");
    run(&mut s, "create view adults as select * from people");
    let tables = run(&mut s, "show tables");
    assert_eq!(tables.columns[1].name, "name");
    assert_eq!(tables.json_rows().unwrap().iter().map(|r| r[1].clone()).collect::<Vec<_>>(), vec![v("PEOPLE")]);
    assert_eq!(
        rows(&mut s, "show terse objects like 'A%'").iter().map(|r| r[1].clone()).collect::<Vec<_>>(),
        vec![v("ADULTS")]
    );
    assert_eq!(rows(&mut s, "show views in schema test.public").len(), 1);
    assert!(rows(&mut s, "show schemas").iter().any(|r| r[1] == v("INFORMATION_SCHEMA")));
    assert!(rows(&mut s, "show databases").iter().any(|r| r[1] == v("TEST")));
    let desc = rows(&mut s, "describe table people");
    assert_eq!(desc[0][..4], [v("ID"), v("NUMBER(38,0)"), v("COLUMN"), v("N")]);
    assert_eq!(desc[2][1], v("NUMBER(10,2)"));
    assert_eq!(fail(&mut s, "desc table nope").code, "002003");
    let columns = rows(&mut s, "show columns in table people");
    assert_eq!(columns[1][2], v("NAME"));
    assert!(columns[1][3].as_deref().unwrap().contains("\"TEXT\""));
    assert_eq!(
        rows(&mut s, "select table_name, table_type from information_schema.tables order by 1"),
        vec![vec![v("ADULTS"), v("VIEW")], vec![v("PEOPLE"), v("BASE TABLE")]]
    );
    assert_eq!(
        rows(
            &mut s,
            "select column_name, data_type, numeric_precision from test.information_schema.columns where table_name = 'PEOPLE' order by ordinal_position"
        ),
        vec![
            vec![v("ID"), v("NUMBER"), v("38")],
            vec![v("NAME"), v("TEXT"), None],
            vec![v("SCORE"), v("NUMBER"), v("10")]
        ]
    );
}

#[test]
fn result_scan() {
    let mut s = session();
    run(&mut s, "create table a (x int)");
    run(&mut s, "create table b (x int)");
    run(&mut s, "show tables");
    assert_eq!(
        rows(&mut s, r#"select "name" from table(result_scan(last_query_id())) order by 1"#),
        vec![vec![v("A")], vec![v("B")]]
    );
}

#[test]
fn session_variables_and_parameters() {
    let mut s = session();
    run(&mut s, "set threshold = 10");
    run(&mut s, "set (a, b) = ('x', 2.5)");
    assert_eq!(value(&mut s, "select $threshold + 1"), v("11"));
    assert_eq!(value(&mut s, "select $a || $b"), v("x2.5"));
    run(&mut s, "unset threshold");
    assert_eq!(fail(&mut s, "select $threshold").code, "002211");
    run(&mut s, "alter session set query_tag = 'etl', timestamp_type_mapping = 'TIMESTAMP_LTZ'");
    assert_eq!(s.ctx.parameters.get("QUERY_TAG").map(String::as_str), Some("etl"));
    assert_eq!(run(&mut s, "select '2024-01-01'::timestamp").columns[0].sf_type, SfType::TimestampLtz);
    assert!(rows(&mut s, "show parameters like 'QUERY_TAG'").iter().any(|r| r[1] == v("etl")));
    assert_eq!(value(&mut s, "select current_user()"), v("TESTER"));
    assert_eq!(value(&mut s, "select current_role()"), v("ACCOUNTADMIN"));
    run(&mut s, "use warehouse wh");
    assert_eq!(value(&mut s, "select current_warehouse()"), v("WH"));
}

#[test]
fn multi_statement_and_binds() {
    let mut s = session();
    let err = s.execute(&Request { sql: "select 1; select 2", ..Default::default() }).unwrap_err();
    assert_eq!(err.error.code, "000008");
    let results = s
        .execute(&Request {
            sql: "create table t (a int, b varchar); insert into t values (1, 'x'); select * from t",
            statement_count: Some(0),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(results.len(), 3);
    let bound = s
        .execute(&Request {
            sql: "insert into t values (?, ?)",
            bindings: vec![vec![Bind::Int(2), Bind::Text("y".into())], vec![Bind::Int(3), Bind::Null]],
            ..Default::default()
        })
        .unwrap();
    assert_eq!(bound[0].json_rows().unwrap(), vec![vec![v("2")]]);
    let selected = s
        .execute(&Request {
            sql: "select b from t where a = :1",
            bindings: vec![vec![Bind::Int(2)]],
            ..Default::default()
        })
        .unwrap();
    assert_eq!(selected[0].json_rows().unwrap(), vec![vec![v("y")]]);
}

#[test]
fn transactions() {
    let mut s = session();
    run(&mut s, "create table t (a int)");
    run(&mut s, "begin");
    run(&mut s, "insert into t values (1)");
    run(&mut s, "rollback");
    assert_eq!(value(&mut s, "select count(*) from t"), v("0"));
    run(&mut s, "begin transaction");
    run(&mut s, "insert into t values (1)");
    run(&mut s, "commit");
    assert_eq!(value(&mut s, "select count(*) from t"), v("1"));
}

#[test]
fn snowflake_query_syntax() {
    let mut s = session();
    run(&mut s, "create table t (g varchar, x int)");
    run(&mut s, "insert into t values ('a', 1), ('a', 2), ('b', 3)");
    assert_eq!(
        rows(&mut s, "select g, x from t qualify row_number() over (partition by g order by x desc) = 1 order by g"),
        vec![vec![v("a"), v("2")], vec![v("b"), v("3")]]
    );
    assert_eq!(rows(&mut s, "select top 1 x from t order by x"), vec![vec![v("1")]]);
    assert_eq!(rows(&mut s, "select x from t minus select x from t where x > 1"), vec![vec![v("1")]]);
    assert_eq!(value(&mut s, "select count_if(x > 1) from t"), v("2"));
    assert_eq!(value(&mut s, "select * exclude (g) from t where x = 3"), v("3"));
    assert_eq!(value(&mut s, "select count(*) from table(generator(rowcount => 5))"), v("5"));
    assert_eq!(value(&mut s, "select sum(x) from identifier('t')"), v("6"));
    run(
        &mut s,
        "merge into t using (select 'c' as g, 4 as x) s on t.g = s.g when matched then update set x = s.x when not matched then insert (g, x) values (s.g, s.x)",
    );
    assert_eq!(value(&mut s, "select x from t where g = 'c'"), v("4"));
}

#[test]
fn arrow_encoding_round_trips() {
    let mut s = session();
    let result = run(
        &mut s,
        "select 1 as i, 2.5::number(10,2) as d, 'x' as t, '2024-01-02 03:04:05'::timestamp as ts, parse_json('{\"a\":1}') as v, null::int as n",
    );
    let encoded = result.arrow_rowset().unwrap();
    let bytes = base64::engine::general_purpose::STANDARD.decode(encoded).unwrap();
    let reader = arrow_ipc::reader::StreamReader::try_new(std::io::Cursor::new(bytes), None).unwrap();
    let schema = reader.schema();
    assert_eq!(schema.field(0).metadata().get("logicalType").map(String::as_str), Some("FIXED"));
    assert_eq!(schema.field(3).data_type(), &DataType::Struct(encode_timestamp_fields()));
    let batches: Vec<_> = reader.collect::<Result<_, _>>().unwrap();
    assert_eq!(batches[0].num_rows(), 1);
}

fn encode_timestamp_fields() -> duckdb::arrow::datatypes::Fields {
    use duckdb::arrow::datatypes::Field;
    vec![Field::new("epoch", DataType::Int64, false), Field::new("fraction", DataType::Int32, false)].into()
}

#[test]
fn transactions_follow_snowflake_rules() {
    let mut s = session();
    run(&mut s, "create table t (a int)");
    run(&mut s, "commit");
    run(&mut s, "rollback");
    run(&mut s, "begin");
    run(&mut s, "insert into t values (1)");
    run(&mut s, "rollback");
    assert_eq!(value(&mut s, "select count(*) from t"), v("0"));
    run(&mut s, "alter session set autocommit = false");
    run(&mut s, "insert into t values (2)");
    run(&mut s, "rollback");
    assert_eq!(value(&mut s, "select count(*) from t"), v("0"));
    run(&mut s, "insert into t values (3)");
    run(&mut s, "create table u (a int)");
    run(&mut s, "rollback");
    assert_eq!(value(&mut s, "select count(*) from t"), v("1"), "DDL committed the insert");
    run(&mut s, "insert into t values (4)");
    run(&mut s, "alter session set autocommit = true");
    run(&mut s, "rollback");
    assert_eq!(value(&mut s, "select count(*) from t"), v("2"), "enabling autocommit committed");
}

#[test]
fn current_date_and_time_functions() {
    let mut s = session();
    for (sql, sf_type) in [
        ("select current_timestamp()", SfType::TimestampLtz),
        ("select current_timestamp", SfType::TimestampLtz),
        ("select getdate()", SfType::TimestampLtz),
        ("select sysdate()", SfType::TimestampNtz),
        ("select current_time()", SfType::Time),
        ("select current_date()", SfType::Date),
    ] {
        let result = run(&mut s, sql);
        assert_eq!(result.columns[0].sf_type, sf_type, "{sql}");
        assert!(value(&mut s, sql).is_some(), "{sql}");
    }
}

#[test]
fn failed_statement_keeps_the_transaction_open() {
    let mut s = session();
    run(&mut s, "create table t (a int)");
    run(&mut s, "begin");
    run(&mut s, "insert into t values (1)");
    // A runtime error aborts DuckDB's transaction; the earlier insert is replayed.
    assert_eq!(fail(&mut s, "insert into t values (1 / 0)").code, "100051");
    run(&mut s, "insert into t values (2)");
    run(&mut s, "commit");
    assert_eq!(value(&mut s, "select count(*) from t"), v("2"));
    // Errors that never reach DuckDB leave the transaction intact too.
    run(&mut s, "begin");
    run(&mut s, "insert into t values (3)");
    fail(&mut s, "create database test");
    run(&mut s, "rollback");
    assert_eq!(value(&mut s, "select count(*) from t"), v("2"));
}

#[test]
fn missing_database_is_ignored_when_configured() {
    let engine = Engine::open(None).unwrap();
    engine.set_create_missing_databases(false);
    let mut s = engine
        .session(SessionOptions { user: "TESTER".into(), database: Some("NOPE".into()), ..Default::default() })
        .unwrap();
    assert_eq!(value(&mut s, "select current_database()"), None);
    assert_eq!(fail(&mut s, "use database nope").code, "002003");
}

#[test]
fn zone_guard_serializes_different_zones() {
    let first = crate::tz::enter("UTC");
    let second = crate::tz::enter("UTC");
    let waiter = std::thread::spawn(|| {
        let _guard = crate::tz::enter("Asia/Tokyo");
        crate::tz::session_zone_now().name().to_owned()
    });
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert_eq!(crate::tz::session_zone_now().name(), "UTC", "Tokyo waits for the UTC statements");
    drop((first, second));
    assert_eq!(waiter.join().unwrap(), "Asia/Tokyo");
}

#[test]
fn show_sequences_hides_identity_sequences() {
    let mut s = session();
    run(&mut s, "create table t (id int autoincrement, a int)");
    run(&mut s, "create sequence seq1");
    let names: Vec<_> = rows(&mut s, "show sequences").into_iter().map(|r| r[1].clone()).collect();
    assert_eq!(names, [v("SEQ1")]);
}

#[test]
fn local_directory_stage_and_put_descriptor() {
    let mut s = session();
    let dir = std::env::temp_dir().join(format!("localflake-test-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("sub/a.csv"), "x|1\ny|2\n").unwrap();
    run(
        &mut s,
        &format!("create stage ext url = 'file://{}' file_format = (type = csv field_delimiter = '|')", dir.display()),
    );
    assert_eq!(
        rows(&mut s, "select $1, $2 from @ext/sub order by 1"),
        vec![vec![v("x"), v("1")], vec![v("y"), v("2")]]
    );
    // PUT only works with internal stages; for those it returns a transfer descriptor.
    assert_eq!(fail(&mut s, "put file:///tmp/a.csv @ext").code, "002003");
    run(&mut s, "create stage internal");
    let put = run(&mut s, "put 'file:///tmp/My File.csv' @internal/In auto_compress = false");
    let transfer = put.transfer.as_ref().unwrap();
    assert!(transfer.upload && !transfer.auto_compress);
    assert_eq!(transfer.src_locations, vec!["/tmp/My File.csv".to_owned()]);
    assert!(transfer.stage_path.ends_with("/stages/INTERNAL/In/"), "{}", transfer.stage_path);
    std::fs::remove_dir_all(dir).unwrap();
}

fn bound(s: &mut Session, sql: &str, binds: Vec<Bind>) -> Vec<Vec<Option<String>>> {
    match s.execute(&Request { sql, bindings: vec![binds], ..Default::default() }) {
        Ok(results) => results[0].json_rows().unwrap(),
        Err(failure) => panic!("{sql}: {}", failure.error),
    }
}

#[test]
fn binds_alongside_flatten_and_escapes() {
    let mut s = session();
    run(&mut s, "create table t (id varchar, account_id varchar, title varchar, ts timestamp_tz)");
    run(
        &mut s,
        r#"insert into t select value:ID::varchar, value:ACCOUNT_ID::varchar, value:TITLE::varchar, value:TS::varchar::timestamp_tz
           from table(flatten(input => parse_json('[{"ID":"1","ACCOUNT_ID":"A","TITLE":"call title 1","TS":"2026-01-01T12:00:00.000+00:00"}]')))"#,
    );
    let formatted = "to_varchar(ts, 'YYYY-MM-DD\"T\"HH24:MI:SS.FF3TZH:TZM')";
    let expected = vec![vec![v("2026-01-01T12:00:00.000+00:00")]];
    assert_eq!(
        bound(&mut s, &format!("select {formatted} from t where id = ?"), vec![Bind::Text("1".into())]),
        expected
    );
    let in_flatten = format!(
        "select {formatted} from t where account_id in (select value::string from table(flatten(input => parse_json(?))))"
    );
    assert_eq!(bound(&mut s, &in_flatten, vec![Bind::Text(r#"["A"]"#.into())]), expected);
    // `'\\'` is one backslash, also once `?` binds are renumbered.
    let ilike = r"select id from t where title ilike ? escape '\\'";
    assert_eq!(bound(&mut s, ilike, vec![Bind::Text("%title 1%".into())]), vec![vec![v("1")]]);
    assert_eq!(bound(&mut s, r"select ? || 'a\\b'", vec![Bind::Text("x".into())]), vec![vec![v(r"xa\b")]]);
}

#[test]
fn text_binds_range_compared_to_dates_and_timestamps() {
    let mut s = session();
    run(&mut s, "create table t (d date, ts timestamp_tz)");
    run(&mut s, "insert into t values ('2026-01-01', '2026-01-01T12:00:00.000+00:00')");
    let formatted = "to_varchar(ts, 'YYYY-MM-DD\"T\"HH24:MI:SS.FF3TZH:TZM')";
    let expected = vec![vec![v("2026-01-01T12:00:00.000+00:00")]];
    for (filter, bind) in [
        ("d >= ?", "2025-10-28"),
        ("d < ?", "2026-10-28"),
        ("ts >= ?", "2026-01-01T00:00:00.000+00:00"),
        ("d = ?", "2026-01-01"),
    ] {
        let sql = format!("select {formatted} from t where {filter}");
        assert_eq!(bound(&mut s, &sql, vec![Bind::Text(bind.into())]), expected, "{filter}");
    }
    // The probe inlines text binds as literals, so quotes are escaped.
    let quoted = format!("select {formatted} || ? from t where d >= ?");
    assert_eq!(
        bound(&mut s, &quoted, vec![Bind::Text(" it's".into()), Bind::Text("2025-10-28".into())]),
        vec![vec![v("2026-01-01T12:00:00.000+00:00 it's")]]
    );
}
