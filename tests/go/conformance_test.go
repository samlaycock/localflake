// Runs the shared driver conformance spec (tests/conformance) through
// gosnowflake, with both Arrow and JSON results.
package e2e

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"math"
	"math/big"
	"os"
	"path/filepath"
	"reflect"
	"sort"
	"strconv"
	"strings"
	"testing"
	"time"

	sf "github.com/snowflakedb/gosnowflake/v2"
)

type spec struct {
	Cases []conformanceCase `json:"cases"`
}

type conformanceCase struct {
	Name    string            `json:"name"`
	Connect connectOptions    `json:"connect"`
	Files   map[string]string `json:"files"`
	Skip    map[string]string `json:"skip"`
	Steps   []step            `json:"steps"`
}

type connectOptions struct {
	Database          string            `json:"database"`
	Schema            string            `json:"schema"`
	Role              string            `json:"role"`
	Warehouse         string            `json:"warehouse"`
	SessionParameters map[string]string `json:"session_parameters"`
}

type expectation struct {
	Columns  [][2]string `json:"columns"`
	Rows     [][]any     `json:"rows"`
	RowCount *int        `json:"row_count"`
	Affected *int64      `json:"rowcount"`
}

type step struct {
	expectation
	SQL        string            `json:"sql"`
	Binds      []bindSpec        `json:"binds"`
	BindRows   [][]bindSpec      `json:"bind_rows"`
	Statements int               `json:"statements"`
	Async      bool              `json:"async"`
	Describe   bool              `json:"describe"`
	Results    []expectation     `json:"results"`
	Error      *errorSpec        `json:"error"`
	FilesAfter map[string]string `json:"files_after"`
}

type bindSpec struct {
	Type  string `json:"type"`
	Value any    `json:"value"`
}

type errorSpec struct {
	Code     string `json:"code"`
	SQLState string `json:"sqlstate"`
}

func TestConformance(t *testing.T) {
	data, err := os.ReadFile("../conformance/cases.json")
	if err != nil {
		t.Fatal(err)
	}
	var s spec
	decoder := json.NewDecoder(strings.NewReader(string(data)))
	decoder.UseNumber()
	if err := decoder.Decode(&s); err != nil {
		t.Fatal(err)
	}
	for _, format := range []string{"ARROW", "JSON"} {
		for _, c := range s.Cases {
			t.Run(format+"/"+c.Name, func(t *testing.T) {
				for _, key := range []string{"go", "go/" + format} {
					if reason, ok := c.Skip[key]; ok {
						t.Skip(reason)
					}
				}
				runCase(t, c, format)
			})
		}
	}
}

func runCase(t *testing.T, c conformanceCase, format string) {
	tmpdir := t.TempDir()
	for name, content := range c.Files {
		if err := os.WriteFile(filepath.Join(tmpdir, name), []byte(content), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	params := map[string]*string{"GO_QUERY_RESULT_FORMAT": &format}
	for k, v := range c.Connect.SessionParameters {
		params[k] = &v
	}
	database := c.Connect.Database
	if database == "" {
		database = "CONFORMANCE"
	}
	cfg := &sf.Config{
		Account: "localflake", User: "conformance", Password: "conformance",
		Host: "127.0.0.1", Port: port, Protocol: "http",
		Database: database, Schema: c.Connect.Schema, Role: c.Connect.Role, Warehouse: c.Connect.Warehouse,
		Params: params,
	}
	dsn, err := sf.DSN(cfg)
	if err != nil {
		t.Fatal(err)
	}
	db, err := sql.Open("snowflake", dsn)
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	ctx := context.Background()
	// One connection, so the steps share a session.
	conn, err := db.Conn(ctx)
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()
	for i, st := range c.Steps {
		if err := runStep(ctx, conn, st, tmpdir); err != nil {
			t.Fatalf("step %d (%s): %v", i+1, st.SQL, err)
		}
	}
}

func runStep(ctx context.Context, conn *sql.Conn, st step, tmpdir string) error {
	query := strings.ReplaceAll(st.SQL, "{tmpdir}", tmpdir)
	for name := range st.FilesAfter {
		if err := os.MkdirAll(filepath.Dir(filepath.Join(tmpdir, name)), 0o755); err != nil {
			return err
		}
	}
	args, err := bindArgs(st)
	if err != nil {
		return err
	}
	switch {
	case st.Describe:
		ctx = sf.WithDescribeOnly(ctx)
	case st.Async:
		ctx = sf.WithAsyncMode(ctx)
	case st.Statements > 0:
		ctx = sf.WithMultiStatement(ctx, st.Statements)
	}

	var results []result
	var runErr error
	if st.Affected != nil && st.Rows == nil && st.Columns == nil && st.Results == nil {
		var res sql.Result
		res, runErr = conn.ExecContext(ctx, query, args...)
		if runErr == nil {
			affected, err := res.RowsAffected()
			if err != nil {
				return err
			}
			results = []result{{affected: &affected}}
		}
	} else {
		results, runErr = queryAll(ctx, conn, query, args, st.Describe)
	}
	if runErr != nil {
		var sfErr *sf.SnowflakeError
		if st.Error == nil {
			return fmt.Errorf("unexpected error: %w", runErr)
		}
		if !errors.As(runErr, &sfErr) {
			return fmt.Errorf("error is not a SnowflakeError: %w", runErr)
		}
		if code := fmt.Sprintf("%06d", sfErr.Number); code != st.Error.Code {
			return fmt.Errorf("error code %s, expected %s (%v)", code, st.Error.Code, runErr)
		}
		if st.Error.SQLState != "" && sfErr.SQLState != st.Error.SQLState {
			return fmt.Errorf("sqlstate %s, expected %s (%v)", sfErr.SQLState, st.Error.SQLState, runErr)
		}
		if sfErr.QueryID == "" && !st.Async {
			return errors.New("errors carry a query ID")
		}
		return nil
	}
	if st.Error != nil {
		return fmt.Errorf("expected error %+v", *st.Error)
	}

	expected := st.Results
	if expected == nil {
		expected = []expectation{st.expectation}
	}
	if len(results) != len(expected) {
		return fmt.Errorf("%d results, expected %d", len(results), len(expected))
	}
	for i, want := range expected {
		if err := results[i].check(want); err != nil {
			if len(expected) > 1 {
				return fmt.Errorf("statement %d: %w", i+1, err)
			}
			return err
		}
	}
	for name, content := range st.FilesAfter {
		got, err := os.ReadFile(filepath.Join(tmpdir, name))
		if err != nil {
			return fmt.Errorf("%s was not downloaded: %w", name, err)
		}
		if string(got) != content {
			return fmt.Errorf("%s: %q", name, got)
		}
	}
	return nil
}

type result struct {
	columns  [][2]string
	rows     [][]any
	affected *int64
}

func queryAll(ctx context.Context, conn *sql.Conn, query string, args []any, describe bool) ([]result, error) {
	rows, err := conn.QueryContext(ctx, query, args...)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var results []result
	for {
		types, err := rows.ColumnTypes()
		if err != nil {
			return nil, err
		}
		var r result
		for _, ct := range types {
			r.columns = append(r.columns, [2]string{ct.Name(), columnType(ct)})
		}
		r.rows = [][]any{}
		for !describe && rows.Next() {
			values := make([]any, len(types))
			pointers := make([]any, len(types))
			for i := range values {
				pointers[i] = &values[i]
			}
			if err := rows.Scan(pointers...); err != nil {
				return nil, err
			}
			row := make([]any, len(types))
			for i, v := range values {
				if row[i], err = canonical(v, r.columns[i][1]); err != nil {
					return nil, fmt.Errorf("column %s: %w", r.columns[i][0], err)
				}
			}
			r.rows = append(r.rows, row)
		}
		if err := rows.Err(); err != nil {
			return nil, err
		}
		// DML in a multi-statement request reports its counts as a result set.
		if len(r.rows) == 1 && len(r.columns) > 0 && strings.HasPrefix(r.columns[0][0], "number of rows") {
			var n int64
			for _, v := range r.rows[0] {
				var x int64
				fmt.Sscan(fmt.Sprint(v), &x)
				n += x
			}
			r.affected = &n
		}
		results = append(results, r)
		if !rows.NextResultSet() {
			break
		}
	}
	return results, nil
}

func (r result) check(want expectation) error {
	if want.Columns != nil {
		if len(r.columns) != len(want.Columns) {
			return fmt.Errorf("columns %v", r.columns)
		}
		for i, c := range r.columns {
			if c[0] != want.Columns[i][0] {
				return fmt.Errorf("column name %q, expected %q", c[0], want.Columns[i][0])
			}
			if !typeMatches(c[1], want.Columns[i][1]) {
				return fmt.Errorf("column %s type %s, expected %s", c[0], c[1], want.Columns[i][1])
			}
		}
	}
	if want.Affected != nil {
		if r.affected == nil || *r.affected != *want.Affected {
			return fmt.Errorf("rowcount %v, expected %d", r.affected, *want.Affected)
		}
	}
	if want.RowCount != nil && len(r.rows) != *want.RowCount {
		return fmt.Errorf("%d rows, expected %d", len(r.rows), *want.RowCount)
	}
	if want.Rows != nil {
		if len(r.rows) != len(want.Rows) {
			return fmt.Errorf("rows %v", r.rows)
		}
		for i, row := range r.rows {
			if !rowEqual(row, want.Rows[i]) {
				return fmt.Errorf("row %v, expected %v", row, want.Rows[i])
			}
		}
	}
	return nil
}

func columnType(ct *sql.ColumnType) string {
	name := ct.DatabaseTypeName()
	if name == "FIXED" {
		precision, scale, _ := ct.DecimalSize()
		return fmt.Sprintf("FIXED(%d,%d)", precision, scale)
	}
	return name
}

func typeMatches(actual, expected string) bool {
	if strings.HasPrefix(expected, "FIXED(*,") && strings.HasPrefix(actual, "FIXED(") {
		return strings.Split(actual, ",")[1] == strings.Split(expected, ",")[1]
	}
	return actual == expected
}

func rowEqual(row, want []any) bool {
	if len(row) != len(want) {
		return false
	}
	for i := range row {
		if n, ok := want[i].(json.Number); ok {
			expected, _ := n.Float64()
			actual, isFloat := row[i].(float64)
			if !isFloat || actual != expected {
				return false
			}
			continue
		}
		if !reflect.DeepEqual(row[i], want[i]) {
			return false
		}
	}
	return true
}

func fraction(seconds, nanos int) string {
	text := fmt.Sprintf("%02d", seconds)
	if nanos != 0 {
		text += strings.TrimRight(fmt.Sprintf(".%09d", nanos), "0")
	}
	return text
}

// canonical converts a gosnowflake value to the spec's canonical form.
func canonical(v any, typ string) (any, error) {
	if v == nil {
		return nil, nil
	}
	base := strings.SplitN(typ, "(", 2)[0]
	switch base {
	case "FIXED":
		var scale int
		fmt.Sscanf(strings.SplitN(typ, ",", 2)[1], "%d)", &scale)
		var text string
		switch x := v.(type) {
		case string:
			text = x
		case int64:
			text = fmt.Sprint(x)
		case float64:
			text = big.NewFloat(x).Text('f', scale)
		case *big.Int:
			text = x.String()
		case *big.Float:
			text = x.Text('f', scale)
		default:
			return nil, fmt.Errorf("unexpected FIXED value %T", v)
		}
		return fixed(text, scale), nil
	case "REAL":
		f, ok := v.(float64)
		if text, isText := v.(string); isText {
			// JSON results scanned into `any` are text; Snowflake writes `inf` and `NaN`.
			var err error
			if f, err = strconv.ParseFloat(text, 64); err != nil {
				return nil, err
			}
			ok = true
		}
		if !ok {
			return nil, fmt.Errorf("unexpected REAL value %T", v)
		}
		switch {
		case math.IsNaN(f):
			return "NaN", nil
		case math.IsInf(f, 1):
			return "Infinity", nil
		case math.IsInf(f, -1):
			return "-Infinity", nil
		}
		return f, nil
	case "DATE", "TIME", "TIMESTAMP_NTZ", "TIMESTAMP_LTZ", "TIMESTAMP_TZ":
		ts, ok := v.(time.Time)
		if !ok {
			return nil, fmt.Errorf("unexpected %s value %T", base, v)
		}
		if base == "TIMESTAMP_LTZ" {
			ts = ts.UTC()
		}
		clock := fmt.Sprintf("%02d:%02d:%s", ts.Hour(), ts.Minute(), fraction(ts.Second(), ts.Nanosecond()))
		switch base {
		case "DATE":
			return ts.Format("2006-01-02"), nil
		case "TIME":
			return clock, nil
		case "TIMESTAMP_LTZ":
			return ts.Format("2006-01-02 ") + clock + "Z", nil
		case "TIMESTAMP_TZ":
			_, offset := ts.Zone()
			sign := "+"
			if offset < 0 {
				sign, offset = "-", -offset
			}
			return fmt.Sprintf("%s%s%s%02d:%02d", ts.Format("2006-01-02 "), clock, sign, offset/3600, offset%3600/60), nil
		}
		return ts.Format("2006-01-02 ") + clock, nil
	case "BOOLEAN":
		switch x := v.(type) {
		case bool:
			return x, nil
		case string:
			return strconv.ParseBool(x)
		}
		return nil, fmt.Errorf("unexpected BOOLEAN value %T", v)
	case "BINARY":
		b, ok := v.([]byte)
		if !ok {
			return nil, fmt.Errorf("unexpected BINARY value %T", v)
		}
		return fmt.Sprintf("%x", b), nil
	case "VARIANT", "OBJECT", "ARRAY", "VECTOR":
		text, ok := v.(string)
		if !ok {
			return nil, fmt.Errorf("unexpected %s value %T", base, v)
		}
		decoder := json.NewDecoder(strings.NewReader(text))
		decoder.UseNumber()
		var parsed any
		if err := decoder.Decode(&parsed); err != nil {
			return nil, err
		}
		return stableJSON(parsed), nil
	}
	return v, nil
}

func fixed(text string, scale int) string {
	negative := strings.HasPrefix(text, "-")
	whole, frac, _ := strings.Cut(strings.TrimPrefix(text, "-"), ".")
	digits := whole
	if scale > 0 {
		digits += "." + (frac + strings.Repeat("0", scale))[:scale]
	}
	if negative {
		return "-" + digits
	}
	return digits
}

func stableJSON(v any) string {
	switch x := v.(type) {
	case map[string]any:
		keys := make([]string, 0, len(x))
		for k := range x {
			keys = append(keys, k)
		}
		sort.Strings(keys)
		parts := make([]string, len(keys))
		for i, k := range keys {
			parts[i] = jsonString(k) + ":" + stableJSON(x[k])
		}
		return "{" + strings.Join(parts, ",") + "}"
	case []any:
		parts := make([]string, len(x))
		for i, item := range x {
			parts[i] = stableJSON(item)
		}
		return "[" + strings.Join(parts, ",") + "]"
	case string:
		return jsonString(x)
	case json.Number:
		return x.String()
	case nil:
		return "null"
	}
	return fmt.Sprint(v)
}

func jsonString(s string) string {
	var b strings.Builder
	encoder := json.NewEncoder(&b)
	encoder.SetEscapeHTML(false)
	_ = encoder.Encode(s)
	return strings.TrimSuffix(b.String(), "\n")
}

// bindArgs converts the step's binds to driver arguments. Dates and times
// are bound as time.Time with gosnowflake's type markers.
func bindArgs(st step) ([]any, error) {
	if st.BindRows != nil {
		columns := make([][]any, len(st.BindRows[0]))
		for _, row := range st.BindRows {
			for i, b := range row {
				v, err := bindValue(b)
				if err != nil {
					return nil, err
				}
				columns[i] = append(columns[i], v)
			}
		}
		args := make([]any, len(columns))
		for i := range columns {
			array, err := sf.Array(&columns[i])
			if err != nil {
				return nil, err
			}
			args[i] = array
		}
		return args, nil
	}
	var args []any
	for _, b := range st.Binds {
		v, err := bindValue(b)
		if err != nil {
			return nil, err
		}
		switch b.Type {
		case "DATE":
			args = append(args, sf.DataTypeDate, v)
		case "TIME":
			args = append(args, sf.DataTypeTime, v)
		case "TIMESTAMP_NTZ":
			args = append(args, sf.DataTypeTimestampNtz, v)
		case "TIMESTAMP_LTZ":
			args = append(args, sf.DataTypeTimestampLtz, v)
		case "TIMESTAMP_TZ":
			args = append(args, sf.DataTypeTimestampTz, v)
		default:
			args = append(args, v)
		}
	}
	return args, nil
}

func bindValue(b bindSpec) (any, error) {
	text := fmt.Sprint(b.Value)
	switch b.Type {
	case "NULL":
		return nil, nil
	case "FIXED":
		if strings.Contains(text, ".") {
			return text, nil
		}
		var n int64
		_, err := fmt.Sscan(text, &n)
		return n, err
	case "REAL":
		return b.Value.(json.Number).Float64()
	case "BOOLEAN":
		return b.Value.(bool), nil
	case "DATE":
		return time.Parse("2006-01-02", text)
	case "TIME":
		return time.Parse("15:04:05.999999999", text)
	case "TIMESTAMP_NTZ":
		return time.Parse("2006-01-02 15:04:05.999999999", text)
	case "TIMESTAMP_LTZ":
		return time.Parse("2006-01-02 15:04:05.999999999Z", text)
	case "TIMESTAMP_TZ":
		return time.Parse("2006-01-02 15:04:05.999999999-07:00", text)
	}
	return text, nil
}
