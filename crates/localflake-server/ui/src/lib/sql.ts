// SQL for the editor and code blocks: highlighting, names, and what a statement reads.

export const KEYWORDS = new Set(
  (
    "ABORT ADD ALL ALTER AND ANY ARRAY AS ASC ASOF AT AUTOINCREMENT BEGIN BETWEEN BREAK BY CALL CASCADE CASE CAST CHANGES " +
    "CLONE CLOSE CLUSTER COLLATE COLUMN COLUMNS COMMENT COMMIT CONNECT CONSTRAINT CONTINUE COPY CREATE CROSS CUBE CURSOR " +
    "DATABASE DATABASES DECLARE DEFAULT DELETE DESC DESCRIBE DISTINCT DO DROP DYNAMIC ELSE ELSEIF END EXCEPT EXCEPTION " +
    "EXCLUDE EXECUTE EXISTS EXIT FALSE FETCH FILE FIRST FLATTEN FOR FORMAT FORMATS FROM FULL FUNCTION FUNCTIONS GET GRANT " +
    "GROUP GROUPING HAVING IDENTITY IF IFF ILIKE IMMEDIATE IN INCREMENT INNER INSERT INTERSECT INTO IS JOIN KEY LANGUAGE " +
    "LAST LATERAL LEFT LET LIKE LIMIT LIST LOOP MATCH_RECOGNIZE MATERIALIZED MERGE MINUS NATURAL NOT NULL NULLS OBJECTS OF " +
    "OFFSET ON OPEN OR ORDER OUT OUTER OVER OVERWRITE PARTITION PIVOT PRIMARY PROCEDURE PROCEDURES PUT QUALIFY RAISE RANGE " +
    "RECURSIVE REFERENCES RENAME REPEAT REPLACE RESULTSET RETURN RETURNS REVOKE RIGHT ROLE ROLLBACK ROLLUP ROW ROWS SAMPLE " +
    "SCHEMA SCHEMAS SELECT SEQUENCE SEQUENCES SESSION SET SHOW STAGE STAGES START SWAP TABLE TABLES TABLESAMPLE TEMP " +
    "TEMPORARY THEN TO TOP TRANSACTION TRANSIENT TRUE TRUNCATE UNDROP UNION UNIQUE UNPIVOT UNSET UNTIL UPDATE USE USING " +
    "VALUES VIEW VIEWS WAREHOUSE WHEN WHERE WHILE WINDOW WITH WORK"
  ).split(" ")
)

const TYPES = new Set(
  (
    "ARRAY BIGINT BINARY BOOLEAN BYTEINT CHAR CHARACTER DATE DATETIME DECIMAL DOUBLE FLOAT FLOAT4 FLOAT8 INT INTEGER NUMBER " +
    "NUMERIC OBJECT REAL SMALLINT STRING TEXT TIME TIMESTAMP TIMESTAMP_LTZ TIMESTAMP_NTZ TIMESTAMP_TZ TINYINT VARBINARY " +
    "VARCHAR VARIANT VECTOR"
  ).split(" ")
)

export const FUNCTIONS = (
  "ABS ANY_VALUE ARRAY_AGG ARRAY_CONSTRUCT ARRAY_CONTAINS ARRAY_SIZE AVG CEIL COALESCE CONCAT CONVERT_TIMEZONE COUNT " +
  "COUNT_IF CURRENT_DATABASE CURRENT_DATE CURRENT_SCHEMA CURRENT_TIMESTAMP CURRENT_USER DATEADD DATEDIFF DATE_TRUNC " +
  "DECODE DENSE_RANK DIV0 FIRST_VALUE FLOOR GET GET_PATH GREATEST HASH IFNULL INITCAP LAG LAST_QUERY_ID LAST_VALUE LEAD " +
  "LEAST LENGTH LISTAGG LOWER LPAD LTRIM MAX MAX_BY MEDIAN MIN MIN_BY MOD NTILE NULLIF NVL NVL2 OBJECT_CONSTRUCT " +
  "OBJECT_KEYS PARSE_JSON PERCENT_RANK RANK REGEXP_LIKE REGEXP_REPLACE REGEXP_SUBSTR REPLACE RESULT_SCAN ROUND " +
  "ROW_NUMBER RPAD RTRIM SEQ4 SPLIT SPLIT_PART SQRT STARTSWITH SUBSTR SUM SYSTEM$TYPEOF TO_CHAR TO_DATE TO_NUMBER " +
  "TO_TIMESTAMP TO_VARCHAR TO_VARIANT TRIM TRY_CAST TRY_PARSE_JSON TRY_TO_NUMBER TYPEOF UPPER UUID_STRING"
).split(" ")

const TOKEN =
  /(--[^\n]*|\/\/[^\n]*|\/\*[\s\S]*?(?:\*\/|$))|('(?:[^'\\]|\\[\s\S]|'')*(?:'|$))|(\$\$[\s\S]*?(?:\$\$|$))|("(?:[^"]|"")*(?:"|$))|(\b\d+(?:\.\d*)?(?:[eE][-+]?\d+)?\b|\.\d+\b)|(:[A-Za-z_][\w$]*|\$[A-Za-z_][\w$]*|\$\d+|\?)|([A-Za-z_][\w$]*)|([\s\S])/gy

/**
 * The syntax colours. The editor is the one custom component, and these are its only palette
 * colours: keywords take the product's accent, the rest are muted hues readable in both themes.
 * Weights stay normal, so highlighted text lines up with the editor's textarea.
 */
const CLASS = {
  comment: "text-muted-foreground italic",
  string: "text-emerald-700 dark:text-emerald-400",
  ident: "text-indigo-700 dark:text-indigo-300",
  number: "text-amber-700 dark:text-amber-400",
  variable: "text-rose-700 dark:text-rose-400",
  keyword: "text-primary",
  type: "text-amber-700 dark:text-amber-400",
  function: "text-violet-700 dark:text-violet-300",
}

const escapeHtml = (s: string) =>
  s.replace(
    /[&<>"]/g,
    (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c] ?? c
  )

export { escapeHtml }

/** SQL as highlighted HTML. */
export function highlight(sql: string) {
  if (sql.length > 300000) return escapeHtml(sql)
  let out = ""
  const span = (kind: keyof typeof CLASS, html: string) =>
    `<span class="${CLASS[kind]}">${html}</span>`
  TOKEN.lastIndex = 0
  let match: RegExpExecArray | null
  while (TOKEN.lastIndex < sql.length && (match = TOKEN.exec(sql))) {
    const [text, comment, string, dollar, quoted, number, variable, word] =
      match
    const html = escapeHtml(text)
    if (comment) out += span("comment", html)
    else if (string || dollar) out += span("string", html)
    else if (quoted) out += span("ident", html)
    else if (number) out += span("number", html)
    else if (variable) out += span("variable", html)
    else if (word) {
      const upper = word.toUpperCase()
      if (KEYWORDS.has(upper)) out += span("keyword", html)
      else if (TYPES.has(upper)) out += span("type", html)
      else if (sql[TOKEN.lastIndex] === "(") out += span("function", html)
      else out += html
    } else out += html
  }
  return out
}

/** A name as SQL: bare if Snowflake would read it back unchanged, else quoted. */
export function ident(name: string) {
  if (/^[A-Z_][A-Z0-9_$]*$/.test(name) && !KEYWORDS.has(name)) return name
  return `"${name.replace(/"/g, '""')}"`
}

export const qualified = (...parts: (string | null | undefined)[]) =>
  parts
    .filter((p): p is string => p != null)
    .map(ident)
    .join(".")

const NAME = /"[^"]+"|[A-Za-z_][\w$]*/g
const unquote = (p: string) =>
  p.startsWith('"') ? p.slice(1, -1) : p.toUpperCase()

/** A dotted name's parts, quotes removed and bare names upper-cased. */
export const nameParts = (text: string) => (text.match(NAME) ?? []).map(unquote)

const RESERVED = new Set(
  "WHERE JOIN ON LEFT RIGHT INNER OUTER FULL CROSS GROUP ORDER LIMIT QUALIFY HAVING UNION SAMPLE NATURAL USING MINUS EXCEPT WINDOW LATERAL".split(
    " "
  )
)

/** Tables a statement reads, by alias and by name (for completing `alias.`). */
export function tableAliases(sql: string) {
  const aliases = new Map<string, string[]>()
  const pattern =
    /\b(?:FROM|JOIN)\s+((?:"[^"]+"|[A-Za-z_][\w$]*)(?:\.(?:"[^"]+"|[A-Za-z_][\w$]*)){0,2})(?:\s+(?:AS\s+)?([A-Za-z_][\w$]*))?/gi
  for (const match of sql.matchAll(pattern)) {
    const parts = nameParts(match[1])
    const alias =
      match[2] && !RESERVED.has(match[2].toUpperCase())
        ? match[2].toUpperCase()
        : null
    aliases.set(parts[parts.length - 1], parts)
    if (alias) aliases.set(alias, parts)
  }
  return aliases
}
