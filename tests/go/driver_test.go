// Drives localflake with gosnowflake, which decodes Arrow results itself.
// Build first with `cargo build`, or point LOCALFLAKE_BIN at a binary.
package e2e

import (
	"context"
	"database/sql"
	"fmt"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"testing"
	"time"

	sf "github.com/snowflakedb/gosnowflake/v2"
)

var port int

func TestMain(m *testing.M) {
	l, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		panic(err)
	}
	port = l.Addr().(*net.TCPAddr).Port
	l.Close()

	binary := os.Getenv("LOCALFLAKE_BIN")
	if binary == "" {
		binary, _ = filepath.Abs("../../target/debug/localflake")
	}
	server := exec.Command(binary, "--addr", fmt.Sprintf("127.0.0.1:%d", port))
	server.Stderr = os.Stderr
	if err := server.Start(); err != nil {
		panic(err)
	}
	deadline := time.Now().Add(10 * time.Second)
	for {
		if c, err := net.Dial("tcp", fmt.Sprintf("127.0.0.1:%d", port)); err == nil {
			c.Close()
			break
		}
		if time.Now().After(deadline) {
			panic("localflake did not start")
		}
		time.Sleep(50 * time.Millisecond)
	}
	code := m.Run()
	server.Process.Kill()
	os.Exit(code)
}

func open(t *testing.T, database string) *sql.DB {
	t.Helper()
	cfg := &sf.Config{
		Account:  "localflake",
		User:     "test",
		Password: "test",
		Host:     "127.0.0.1",
		Port:     port,
		Protocol: "http",
		Database: database,
	}
	dsn, err := sf.DSN(cfg)
	if err != nil {
		t.Fatal(err)
	}
	db, err := sql.Open("snowflake", dsn)
	if err != nil {
		t.Fatal(err)
	}
	db.SetMaxOpenConns(1)
	t.Cleanup(func() { db.Close() })
	return db
}

func TestArrowTypes(t *testing.T) {
	db := open(t, "GO_TYPES")
	var (
		i  int64
		n  string
		f  float64
		s  string
		b  bool
		d  time.Time
		ts time.Time
		v  string
		nl sql.NullString
	)
	err := db.QueryRow(`select 42, 12.34::number(10,2), 1.5::float, 'héllo', true, '2024-02-29'::date,
		'2024-01-02 03:04:05.678901'::timestamp_ntz, parse_json('[1,2]'), null::varchar`).
		Scan(&i, &n, &f, &s, &b, &d, &ts, &v, &nl)
	if err != nil {
		t.Fatal(err)
	}
	if i != 42 || n != "12.34" || f != 1.5 || s != "héllo" || !b || nl.Valid {
		t.Fatalf("got %v %v %v %v %v %v", i, n, f, s, b, nl)
	}
	if got := d.Format("2006-01-02"); got != "2024-02-29" {
		t.Fatalf("date: %s", got)
	}
	if got := ts.Format("2006-01-02 15:04:05.000000"); got != "2024-01-02 03:04:05.678901" {
		t.Fatalf("timestamp: %s", got)
	}
	if v != "[\n  1,\n  2\n]" {
		t.Fatalf("variant: %q", v)
	}
}

func TestDMLAndBinds(t *testing.T) {
	db := open(t, "GO_DML")
	if _, err := db.Exec("create table t (id int, name varchar)"); err != nil {
		t.Fatal(err)
	}
	res, err := db.Exec("insert into t values (?, ?), (?, ?)", 1, "a", 2, nil)
	if err != nil {
		t.Fatal(err)
	}
	if n, _ := res.RowsAffected(); n != 2 {
		t.Fatalf("rows affected: %d", n)
	}
	var name string
	if err := db.QueryRow("select name from t where id = ?", 1).Scan(&name); err != nil || name != "a" {
		t.Fatalf("%v %q", err, name)
	}
	res, err = db.Exec("delete from t where name is null")
	if err != nil {
		t.Fatal(err)
	}
	if n, _ := res.RowsAffected(); n != 1 {
		t.Fatalf("deleted: %d", n)
	}
}

func TestErrors(t *testing.T) {
	db := open(t, "GO_ERR")
	_, err := db.Query("select * from missing_table")
	sfErr, ok := err.(*sf.SnowflakeError)
	if !ok {
		t.Fatalf("not a SnowflakeError: %v", err)
	}
	if sfErr.Number != 2003 || sfErr.SQLState != "42S02" {
		t.Fatalf("got %d %s", sfErr.Number, sfErr.SQLState)
	}
}

func TestLargeResult(t *testing.T) {
	db := open(t, "GO_LARGE")
	rows, err := db.Query("select seq4(), uuid_string() from table(generator(rowcount => 100000))")
	if err != nil {
		t.Fatal(err)
	}
	defer rows.Close()
	var count, sum int64
	for rows.Next() {
		var i int64
		var u string
		if err := rows.Scan(&i, &u); err != nil {
			t.Fatal(err)
		}
		count++
		sum += i
	}
	if err := rows.Err(); err != nil {
		t.Fatal(err)
	}
	if count != 100000 || sum != 99999*100000/2 {
		t.Fatalf("count %d sum %d", count, sum)
	}
}

func TestTimestampTz(t *testing.T) {
	db := open(t, "GO_TZ")
	var ts time.Time
	if err := db.QueryRow("select '2024-03-15 10:11:12 +02:00'::timestamp_tz").Scan(&ts); err != nil {
		t.Fatal(err)
	}
	_, offset := ts.Zone()
	if got := ts.Format("2006-01-02 15:04:05"); got != "2024-03-15 10:11:12" || offset != 7200 {
		t.Fatalf("got %s offset %d", got, offset)
	}
}

func TestStages(t *testing.T) {
	db := open(t, "GO_STAGES")
	dir := t.TempDir()
	source := filepath.Join(dir, "people.csv")
	if err := os.WriteFile(source, []byte("1,Ada\n2,Grace\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	mustExec := func(query string) {
		t.Helper()
		if _, err := db.Exec(query); err != nil {
			t.Fatalf("%s: %v", query, err)
		}
	}
	mustExec("create or replace stage fixtures")
	mustExec(fmt.Sprintf("put file://%s @fixtures/in/", source))
	var name string
	var size int64
	var md5, modified string
	if err := db.QueryRow("list @fixtures").Scan(&name, &size, &md5, &modified); err != nil {
		t.Fatal(err)
	}
	if name != "fixtures/in/people.csv.gz" {
		t.Fatalf("listed %q", name)
	}
	mustExec("create or replace table people (id int, name varchar)")
	mustExec("copy into people from @fixtures/in/")
	var count int
	if err := db.QueryRow("select count(*) from people").Scan(&count); err != nil || count != 2 {
		t.Fatalf("count %d err %v", count, err)
	}
	mustExec("copy into @fixtures/out/ from people file_format = (type = csv compression = none) single = true")
	out := filepath.Join(dir, "out")
	if err := os.Mkdir(out, 0o755); err != nil {
		t.Fatal(err)
	}
	mustExec(fmt.Sprintf("get @fixtures/out/ file://%s/", out))
	got, err := os.ReadFile(filepath.Join(out, "data"))
	if err != nil || string(got) != "1,Ada\n2,Grace\n" {
		t.Fatalf("downloaded %q err %v", got, err)
	}
}

func TestTransactionAPI(t *testing.T) {
	db := open(t, "GO_TX")
	ctx := context.Background()
	if _, err := db.Exec("create or replace table t (id int)"); err != nil {
		t.Fatal(err)
	}
	tx, err := db.BeginTx(ctx, nil)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := tx.Exec("insert into t values (1)"); err != nil {
		t.Fatal(err)
	}
	if err := tx.Rollback(); err != nil {
		t.Fatal(err)
	}
	tx, err = db.BeginTx(ctx, nil)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := tx.Exec("insert into t values (2)"); err != nil {
		t.Fatal(err)
	}
	if err := tx.Commit(); err != nil {
		t.Fatal(err)
	}
	var ids []int
	rows, err := db.Query("select id from t")
	if err != nil {
		t.Fatal(err)
	}
	for rows.Next() {
		var id int
		if err := rows.Scan(&id); err != nil {
			t.Fatal(err)
		}
		ids = append(ids, id)
	}
	if len(ids) != 1 || ids[0] != 2 {
		t.Fatalf("ids %v", ids)
	}
}

func TestQueryID(t *testing.T) {
	db := open(t, "GO_QID")
	ids := make(chan string, 1)
	ctx := sf.WithQueryIDChan(context.Background(), ids)
	var answer int
	if err := db.QueryRowContext(ctx, "select 42").Scan(&answer); err != nil {
		t.Fatal(err)
	}
	id := <-ids
	var again int
	if err := db.QueryRow(fmt.Sprintf("select * from table(result_scan('%s'))", id)).Scan(&again); err != nil || again != 42 {
		t.Fatalf("result_scan(%s): %d %v", id, again, err)
	}
}

// endless runs for minutes unless the server stops it.
const endless = "select count(*) from table(generator(rowcount => 100000000000)) where uniform(1, 10, random()) > 0"

func TestCancel(t *testing.T) {
	db := open(t, "GO_CANCEL")
	conn, err := db.Conn(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()
	ctx, cancel := context.WithTimeout(context.Background(), 500*time.Millisecond)
	defer cancel()
	var n int
	if err := conn.QueryRowContext(ctx, endless).Scan(&n); err == nil {
		t.Fatal("the query should be cancelled")
	}
	// The session runs one statement at a time, so this only returns quickly
	// if the abort request stopped the endless query on the server.
	started := time.Now()
	if err := conn.QueryRowContext(context.Background(), "select 1").Scan(&n); err != nil || n != 1 {
		t.Fatalf("got %d, %v", n, err)
	}
	if elapsed := time.Since(started); elapsed > 5*time.Second {
		t.Fatalf("the next query waited %v", elapsed)
	}
}

func TestStatementTimeout(t *testing.T) {
	db := open(t, "GO_TIMEOUT")
	if _, err := db.Exec("alter session set statement_timeout_in_seconds = 1"); err != nil {
		t.Fatal(err)
	}
	_, err := db.Query(endless)
	sfErr, ok := err.(*sf.SnowflakeError)
	if !ok {
		t.Fatalf("not a SnowflakeError: %v", err)
	}
	if sfErr.Number != 630 || sfErr.SQLState != "57014" {
		t.Fatalf("got %d %s", sfErr.Number, sfErr.SQLState)
	}
}
