// localflake web UI: SQL worksheets, an object explorer, saved queries, the
// query history of every session and the connected sessions.
//
// Plain JavaScript with no dependencies or build step: the page is embedded
// in the localflake binary, which never needs the network.
"use strict";

(() => {
  // ---------------------------------------------------------------------------
  // Utilities

  const $ = (selector, root = document) => root.querySelector(selector);
  const isMac = /Mac|iPhone|iPad/.test(navigator.platform);
  const MOD = isMac ? "⌘" : "Ctrl";
  const ROW_H = 26;

  /** Creates an element: `h("div.a.b", {title: "x", onClick}, child, ...)`. */
  function h(tag, props, ...children) {
    const [name, ...classes] = tag.split(".");
    const el = document.createElement(name || "div");
    if (classes.length) el.className = classes.join(" ");
    for (const [key, value] of Object.entries(props || {})) {
      if (value == null || value === false) continue;
      if (key === "class") el.className = [el.className, value].filter(Boolean).join(" ");
      else if (key === "style" && typeof value === "object") Object.assign(el.style, value);
      else if (key.startsWith("on") && typeof value === "function") el.addEventListener(key.slice(2).toLowerCase(), value);
      else if (key === "html") el.innerHTML = value;
      else if (key === "value") el.value = value;
      else if (key === "checked" || key === "disabled" || key === "selected") el[key] = value;
      else el.setAttribute(key, value === true ? "" : value);
    }
    append(el, children);
    return el;
  }

  function append(el, children) {
    for (const child of children.flat(Infinity)) {
      if (child == null || child === false) continue;
      el.append(child instanceof Node ? child : document.createTextNode(String(child)));
    }
    return el;
  }

  function clear(el, ...children) {
    el.replaceChildren();
    return append(el, children);
  }

  const escapeHtml = (s) => s.replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);
  const uid = () => (crypto.randomUUID ? crypto.randomUUID() : Math.random().toString(36).slice(2) + Date.now().toString(36));
  const plural = (n, word, many = word + "s") => `${n.toLocaleString()} ${n === 1 ? word : many}`;
  const debounce = (fn, ms) => {
    let timer;
    return (...args) => {
      clearTimeout(timer);
      timer = setTimeout(() => fn(...args), ms);
    };
  };

  const ICONS = {
    play: '<path d="M7 4.5v15l12-7.5z" fill="currentColor" stroke="none"/>',
    playAll: '<path d="M4 5v14l9-7zM13 5v14l9-7z" fill="currentColor" stroke="none"/>',
    stop: '<rect x="6" y="6" width="12" height="12" rx="1.5" fill="currentColor" stroke="none"/>',
    plus: '<path d="M12 5v14M5 12h14"/>',
    x: '<path d="M6 6l12 12M18 6L6 18"/>',
    refresh: '<path d="M20 11a8 8 0 1 0-2.3 5.7M20 4v7h-7"/>',
    save: '<path d="M5 3h11l4 4v13a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1z"/><path d="M8 3v5h7V3M7 21v-7h10v7"/>',
    db: '<ellipse cx="12" cy="5.5" rx="7.5" ry="2.8"/><path d="M4.5 5.5v13c0 1.5 3.4 2.8 7.5 2.8s7.5-1.3 7.5-2.8v-13M4.5 12c0 1.5 3.4 2.8 7.5 2.8s7.5-1.3 7.5-2.8"/>',
    schema: '<path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>',
    table: '<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M3 9.5h18M3 15h18M9 9.5V20"/>',
    view: '<path d="M2 12s3.6-7 10-7 10 7 10 7-3.6 7-10 7S2 12 2 12z"/><circle cx="12" cy="12" r="3"/>',
    column: '<path d="M5 5h14M5 12h14M5 19h9"/>',
    proc: '<path d="M8 6l-6 6 6 6M16 6l6 6-6 6M13.5 4l-3 16"/>',
    func: '<path d="M14 4h-2a3 3 0 0 0-3 3v13M6 10h7M15 13l5 6M20 13l-5 6"/>',
    stage: '<path d="M4 15v4a1 1 0 0 0 1 1h14a1 1 0 0 0 1-1v-4M12 3v12M7 8l5-5 5 5"/>',
    seq: '<path d="M5 6h3v12M5 18h6M14 7a3 3 0 1 1 5 2.2L14 18h6"/>',
    format: '<path d="M14 3H6a1 1 0 0 0-1 1v16a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1V8z"/><path d="M14 3v5h5M9 13h6M9 17h6"/>',
    chevron: '<path d="M9 6l6 6-6 6"/>',
    more: '<circle cx="5" cy="12" r="1.6" fill="currentColor"/><circle cx="12" cy="12" r="1.6" fill="currentColor"/><circle cx="19" cy="12" r="1.6" fill="currentColor"/>',
    copy: '<rect x="8" y="8" width="13" height="13" rx="2"/><path d="M16 8V5a2 2 0 0 0-2-2H5a2 2 0 0 0-2 2v9a2 2 0 0 0 2 2h3"/>',
    download: '<path d="M12 3v12M7 10l5 5 5-5M4 19h16"/>',
    upload: '<path d="M12 21V9M7 14l5-5 5 5M4 5h16"/>',
    trash: '<path d="M4 7h16M10 11v6M14 11v6M6 7l1 13h10l1-13M9 7V4h6v3"/>',
    sun: '<circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4"/>',
    moon: '<path d="M20 14.5A8 8 0 0 1 9.5 4a8 8 0 1 0 10.5 10.5z"/>',
    keyboard: '<rect x="2" y="6" width="20" height="12" rx="2"/><path d="M6 10h.01M10 10h.01M14 10h.01M18 10h.01M7 14h10"/>',
    edit: '<path d="M4 20h4L19 9l-4-4L4 16z"/>',
    insert: '<path d="M4 6h16M4 12h10M4 18h7M17 14v6M14 17h6"/>',
    eraser: '<path d="M7 21h10M5.5 13.5l7-7a2 2 0 0 1 2.8 0l3.2 3.2a2 2 0 0 1 0 2.8L12 19H8.5l-3-3a1.8 1.8 0 0 1 0-2.5z"/>',
    list: '<path d="M9 6h11M9 12h11M9 18h11M4 6h.01M4 12h.01M4 18h.01"/>',
    hash: '<path d="M5 9h14M5 15h14M10 4L8 20M16 4l-2 16"/>',
    info: '<circle cx="12" cy="12" r="9"/><path d="M12 11v5M12 8h.01"/>',
    open: '<path d="M14 4h6v6M20 4l-9 9M18 14v5a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V7a1 1 0 0 1 1-1h5"/>',
    pause: '<path d="M8 5v14M16 5v14"/>',
    filter: '<path d="M4 5h16l-6 7.5V19l-4 2v-8.5z"/>',
    target: '<circle cx="12" cy="12" r="8"/><circle cx="12" cy="12" r="3"/>',
  };

  function icon(name, cls) {
    const span = document.createElement("span");
    span.style.display = "contents";
    span.innerHTML = `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" class="i${cls ? " " + cls : ""}">${ICONS[name] || ""}</svg>`;
    return span.firstChild;
  }

  const iconButton = (name, title, onClick, extra = {}) =>
    h("button.icon-btn", { title, type: "button", onClick: (e) => { e.stopPropagation(); onClick(e); }, ...extra }, icon(name));

  // ---------------------------------------------------------------------------
  // Persistence (the browser's localStorage)

  const store = {
    get(key, fallback) {
      try {
        const value = localStorage.getItem("localflake." + key);
        return value == null ? fallback : JSON.parse(value);
      } catch {
        return fallback;
      }
    },
    set(key, value) {
      try {
        localStorage.setItem("localflake." + key, JSON.stringify(value));
      } catch (err) {
        console.warn("cannot save", key, err);
      }
    },
  };

  /** Worksheet ID → its session `{token, id}`, kept for this browser tab's lifetime. */
  const worksheetSessions = {
    all: (() => {
      try {
        return JSON.parse(sessionStorage.getItem("localflake.sessions") || "{}");
      } catch {
        return {};
      }
    })(),
    get(id) {
      return this.all[id];
    },
    set(id, value) {
      if (value) this.all[id] = value;
      else delete this.all[id];
      try {
        sessionStorage.setItem("localflake.sessions", JSON.stringify(this.all));
      } catch {}
    },
  };

  // ---------------------------------------------------------------------------
  // API

  class ApiError extends Error {
    constructor(message, status, data) {
      super(message);
      this.status = status;
      this.data = data;
      this.code = data?.error?.code;
    }
  }

  async function api(path, { method = "GET", body, raw } = {}) {
    const options = { method, headers: {} };
    if (raw !== undefined) {
      options.body = raw;
      options.headers["Content-Type"] = "application/octet-stream";
    } else if (body !== undefined) {
      options.body = JSON.stringify(body);
      options.headers["Content-Type"] = "application/json";
    }
    let response;
    try {
      response = await fetch("/__localflake/api" + path, options);
    } catch (err) {
      throw new ApiError("localflake isn't responding. Is it still running?", 0, null);
    }
    let data = {};
    try {
      data = await response.json();
    } catch {}
    if (!response.ok) throw new ApiError(data?.error?.message || `${response.status} ${response.statusText}`, response.status, data);
    return data;
  }

  // ---------------------------------------------------------------------------
  // Toasts, menus and dialogs

  function toast(message, kind = "") {
    const el = h("div.toast", { class: kind }, message);
    $("#toasts").append(el);
    setTimeout(() => el.remove(), kind === "error" ? 6000 : 2600);
  }

  async function copyText(text, what = "Copied") {
    try {
      await navigator.clipboard.writeText(text);
    } catch {
      const area = h("textarea", { style: { position: "fixed", opacity: "0" } });
      area.value = text;
      document.body.append(area);
      area.select();
      document.execCommand("copy");
      area.remove();
    }
    toast(what);
  }

  function download(filename, text, type = "text/plain") {
    const url = URL.createObjectURL(new Blob([text], { type }));
    const a = h("a", { href: url, download: filename });
    document.body.append(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  }

  let openMenuEl = null;
  function closeMenu() {
    openMenuEl?.remove();
    openMenuEl = null;
  }

  /** Shows a context menu of `{label, icon, danger, action}` items (null = separator). */
  function showMenu(x, y, items, title) {
    closeMenu();
    const menu = h("div.menu", { onContextmenu: (e) => e.preventDefault() });
    if (title) menu.append(h("div.menu-title", null, title));
    for (const item of items) {
      if (!item) {
        if (menu.lastChild && menu.lastChild.tagName !== "HR") menu.append(h("hr"));
        continue;
      }
      menu.append(
        h("button", { class: item.danger ? "danger" : "", type: "button", onClick: () => { closeMenu(); item.action(); } },
          icon(item.icon || "chevron"), item.label),
      );
    }
    document.body.append(menu);
    const rect = menu.getBoundingClientRect();
    menu.style.left = Math.min(x, innerWidth - rect.width - 8) + "px";
    menu.style.top = Math.min(y, innerHeight - rect.height - 8) + "px";
    openMenuEl = menu;
  }
  document.addEventListener("mousedown", (e) => { if (openMenuEl && !openMenuEl.contains(e.target)) closeMenu(); });
  document.addEventListener("scroll", closeMenu, true);

  /** Opens a modal dialog; resolves with whatever `close(value)` is called with. */
  function dialog({ title, body, buttons = [], wide = false, onOpen }) {
    return new Promise((resolve) => {
      const previous = document.activeElement;
      const close = (value) => {
        backdrop.remove();
        document.removeEventListener("keydown", onKey, true);
        previous?.focus?.();
        resolve(value);
      };
      const onKey = (e) => {
        if (e.key === "Escape") {
          e.stopPropagation();
          close(null);
        } else if (e.key === "Enter" && (e.metaKey || e.ctrlKey || e.target.tagName === "INPUT")) {
          const primary = buttons.find((b) => b.primary);
          if (primary && !primary.disabled?.()) {
            e.preventDefault();
            primary.onClick(close);
          }
        }
      };
      const foot = h("div.foot", null,
        buttons.map((b) => {
          if ("left" in b) return h("span.left", null, b.left);
          const button = h("button.btn", { type: "button", class: [b.primary && "primary", b.danger && "danger solid"].filter(Boolean).join(" "), onClick: () => b.onClick(close) }, b.label);
          b.element = button;
          return button;
        }),
      );
      const modal = h("div.modal", { class: wide ? "wide" : "" },
        h("h2", null, title, iconButton("x", "Close (Esc)", () => close(null))),
        h("div.body", null, body),
        buttons.length ? foot : null,
      );
      const backdrop = h("div.backdrop", { onMousedown: (e) => { if (e.target === backdrop) close(null); } }, modal);
      document.body.append(backdrop);
      document.addEventListener("keydown", onKey, true);
      const focus = modal.querySelector("input:not([type=file]):not([type=checkbox]), textarea, select") || modal.querySelector(".btn.primary");
      focus?.focus();
      onOpen?.({ close, modal });
    });
  }

  function confirmDialog({ title, message, sql, confirmLabel = "Confirm", danger = false }) {
    return dialog({
      title,
      body: [
        message ? h("div", null, message) : null,
        sql ? h("div.codeblock", null, h("pre", { html: highlight(sql) })) : null,
      ],
      buttons: [
        { label: "Cancel", onClick: (close) => close(false) },
        { label: confirmLabel, primary: !danger, danger, onClick: (close) => close(true) },
      ],
    });
  }

  function promptDialog({ title, label, value = "", placeholder = "", help, confirmLabel = "OK" }) {
    const input = h("input", { type: "text", value, placeholder, spellcheck: "false" });
    setTimeout(() => input.select());
    return dialog({
      title,
      body: h("label.field", null, h("span", null, label), input, help ? h("span.help", null, help) : null),
      buttons: [
        { label: "Cancel", onClick: (close) => close(null) },
        { label: confirmLabel, primary: true, onClick: (close) => close(input.value.trim() || null) },
      ],
    });
  }

  function showValue(column, value, display) {
    let text = display ?? "NULL";
    if (value != null && ["variant", "object", "array"].includes(column.type)) text = value;
    else if (value != null && column.type === "text") {
      try {
        const parsed = JSON.parse(value);
        if (parsed && typeof parsed === "object") text = JSON.stringify(parsed, null, 2);
      } catch {}
    }
    dialog({
      title: [column.name, h("span.chip", { style: { marginLeft: "8px" } }, typeLabel(column))],
      wide: true,
      body: h("pre.value-view", { class: value == null ? "faint" : "" }, text),
      buttons: [
        { left: value == null ? "" : plural(text.length, "character") },
        { label: "Copy", onClick: () => copyText(display ?? "", "Value copied") },
        { label: "Close", primary: true, onClick: (close) => close() },
      ],
    });
  }

  // ---------------------------------------------------------------------------
  // Values: the JSON wire encoding (what drivers receive) to display text

  const pad = (n, width = 2) => String(n).padStart(width, "0");

  /** Seconds since the epoch with nine fractional digits, possibly negative. */
  function parseEpoch(text) {
    const negative = text.startsWith("-");
    const [whole, frac = ""] = (negative ? text.slice(1) : text).split(".");
    let total = BigInt(whole || "0") * 1000000000n + BigInt((frac + "000000000").slice(0, 9));
    if (negative) total = -total;
    let seconds = total / 1000000000n;
    let nanos = total % 1000000000n;
    if (nanos < 0n) {
      nanos += 1000000000n;
      seconds -= 1n;
    }
    return { seconds: Number(seconds), nanos: Number(nanos) };
  }

  /** Fractional seconds: at least three digits, more if they're significant. */
  function fraction(nanos, min = 3) {
    let digits = pad(nanos, 9).replace(/0+$/, "");
    while (digits.length < min) digits += "0";
    return digits;
  }

  function utcDate(date) {
    const year = date.getUTCFullYear();
    const y = year < 0 ? "-" + pad(-year, 4) : pad(year, 4);
    return `${y}-${pad(date.getUTCMonth() + 1)}-${pad(date.getUTCDate())}`;
  }

  const utcTime = (date) => `${pad(date.getUTCHours())}:${pad(date.getUTCMinutes())}:${pad(date.getUTCSeconds())}`;

  function offsetText(minutes) {
    const abs = Math.abs(minutes);
    return `${minutes < 0 ? "-" : "+"}${pad(Math.floor(abs / 60))}${pad(abs % 60)}`;
  }

  const zoneFormats = new Map();
  /** The UTC offset of an IANA time zone at an instant, in minutes. */
  function zoneOffset(millis, zone) {
    if (!zone || zone === "UTC") return 0;
    if (!zoneFormats.has(zone)) {
      let format = null;
      try {
        format = new Intl.DateTimeFormat("en-US", {
          timeZone: zone, hourCycle: "h23", era: "short",
          year: "numeric", month: "numeric", day: "numeric", hour: "numeric", minute: "numeric", second: "numeric",
        });
      } catch {}
      zoneFormats.set(zone, format);
    }
    const format = zoneFormats.get(zone);
    if (!format) return 0;
    const parts = {};
    for (const part of format.formatToParts(new Date(millis))) parts[part.type] = part.value;
    let year = Number(parts.year);
    if (/^B/.test(parts.era || "")) year = 1 - year;
    const wall = new Date(0);
    wall.setUTCFullYear(year, Number(parts.month) - 1, Number(parts.day));
    wall.setUTCHours(Number(parts.hour) % 24, Number(parts.minute), Number(parts.second), 0);
    return Math.round((wall.getTime() - Math.floor(millis / 1000) * 1000) / 60000);
  }

  /** A value as text, the way Snowflake's worksheets show it (null stays null). */
  function formatValue(value, column, timezone) {
    if (value == null) return null;
    try {
      switch (column.type) {
        case "boolean":
          return value === "1" ? "TRUE" : value === "0" ? "FALSE" : value;
        case "date":
          return utcDate(new Date(Number(value) * 86400000));
        case "time": {
          const { seconds, nanos } = parseEpoch(value);
          const text = utcTime(new Date(seconds * 1000));
          return nanos ? `${text}.${fraction(nanos, 0)}` : text;
        }
        case "timestamp_ntz": {
          const { seconds, nanos } = parseEpoch(value);
          const date = new Date(seconds * 1000);
          return `${utcDate(date)} ${utcTime(date)}.${fraction(nanos)}`;
        }
        case "timestamp_ltz": {
          const { seconds, nanos } = parseEpoch(value);
          const offset = zoneOffset(seconds * 1000, timezone);
          const date = new Date((seconds + offset * 60) * 1000);
          return `${utcDate(date)} ${utcTime(date)}.${fraction(nanos)} ${offsetText(offset)}`;
        }
        case "timestamp_tz": {
          const [epoch, zone] = value.split(" ");
          const { seconds, nanos } = parseEpoch(epoch);
          const offset = Number(zone ?? 1440) - 1440;
          const date = new Date((seconds + offset * 60) * 1000);
          return `${utcDate(date)} ${utcTime(date)}.${fraction(nanos)} ${offsetText(offset)}`;
        }
        default:
          return value;
      }
    } catch {
      return value;
    }
  }

  /** `NUMBER(10,2)`, `VARCHAR(20)`, `TIMESTAMP_LTZ`... for result columns. */
  function typeLabel(column) {
    switch (column.type) {
      case "fixed":
        return column.scale ? `NUMBER(${column.precision ?? 38},${column.scale})` : "NUMBER";
      case "real":
        return "FLOAT";
      case "text":
        return column.length && column.length !== 16777216 ? `VARCHAR(${column.length})` : "VARCHAR";
      case "vector":
        return `VECTOR(${column.fields?.[0]?.type === "fixed" ? "INT" : "FLOAT"}, ${column.vectorDimension})`;
      default:
        return column.type.toUpperCase();
    }
  }

  const isNumeric = (column) => column.type === "fixed" || column.type === "real";
  const isJson = (column) => ["variant", "object", "array"].includes(column.type);

  /** Compares wire values of a column for sorting (numbers and instants numerically). */
  function compareValues(a, b, column) {
    if (a == null || b == null) return a == null ? (b == null ? 0 : 1) : -1;
    if (isNumeric(column) || ["date", "time", "timestamp_ntz", "timestamp_ltz", "timestamp_tz"].includes(column.type)) {
      const x = parseFloat(a), y = parseFloat(b);
      if (!Number.isNaN(x) && !Number.isNaN(y) && x !== y) return x - y;
    }
    return a < b ? -1 : a > b ? 1 : 0;
  }

  // ---------------------------------------------------------------------------
  // SQL: highlighting, identifiers

  const KEYWORDS = new Set((
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
  ).split(" "));
  const TYPES = new Set((
    "ARRAY BIGINT BINARY BOOLEAN BYTEINT CHAR CHARACTER DATE DATETIME DECIMAL DOUBLE FLOAT FLOAT4 FLOAT8 INT INTEGER NUMBER " +
    "NUMERIC OBJECT REAL SMALLINT STRING TEXT TIME TIMESTAMP TIMESTAMP_LTZ TIMESTAMP_NTZ TIMESTAMP_TZ TINYINT VARBINARY " +
    "VARCHAR VARIANT VECTOR"
  ).split(" "));
  const FUNCTIONS = (
    "ABS ANY_VALUE ARRAY_AGG ARRAY_CONSTRUCT ARRAY_CONTAINS ARRAY_SIZE AVG CEIL COALESCE CONCAT CONVERT_TIMEZONE COUNT " +
    "COUNT_IF CURRENT_DATABASE CURRENT_DATE CURRENT_SCHEMA CURRENT_TIMESTAMP CURRENT_USER DATEADD DATEDIFF DATE_TRUNC " +
    "DECODE DENSE_RANK DIV0 FIRST_VALUE FLOOR GET GET_PATH GREATEST HASH IFNULL INITCAP LAG LAST_QUERY_ID LAST_VALUE LEAD " +
    "LEAST LENGTH LISTAGG LOWER LPAD LTRIM MAX MAX_BY MEDIAN MIN MIN_BY MOD NTILE NULLIF NVL NVL2 OBJECT_CONSTRUCT " +
    "OBJECT_KEYS PARSE_JSON PERCENT_RANK RANK REGEXP_LIKE REGEXP_REPLACE REGEXP_SUBSTR REPLACE RESULT_SCAN ROUND " +
    "ROW_NUMBER RPAD RTRIM SEQ4 SPLIT SPLIT_PART SQRT STARTSWITH SUBSTR SUM SYSTEM$TYPEOF TO_CHAR TO_DATE TO_NUMBER " +
    "TO_TIMESTAMP TO_VARCHAR TO_VARIANT TRIM TRY_CAST TRY_PARSE_JSON TRY_TO_NUMBER TYPEOF UPPER UUID_STRING"
  ).split(" ");

  const TOKEN = /(--[^\n]*|\/\/[^\n]*|\/\*[\s\S]*?(?:\*\/|$))|('(?:[^'\\]|\\[\s\S]|'')*(?:'|$))|(\$\$[\s\S]*?(?:\$\$|$))|("(?:[^"]|"")*(?:"|$))|(\b\d+(?:\.\d*)?(?:[eE][-+]?\d+)?\b|\.\d+\b)|(:[A-Za-z_][\w$]*|\$[A-Za-z_][\w$]*|\$\d+|\?)|([A-Za-z_][\w$]*)|([\s\S])/gy;

  /** SQL as highlighted HTML. */
  function highlight(sql) {
    if (sql.length > 300000) return escapeHtml(sql);
    let out = "";
    TOKEN.lastIndex = 0;
    let match;
    while (TOKEN.lastIndex < sql.length && (match = TOKEN.exec(sql))) {
      const [text, comment, string, dollar, quoted, number, variable, word] = match;
      const html = escapeHtml(text);
      if (comment) out += `<span class="t-com">${html}</span>`;
      else if (string || dollar) out += `<span class="t-str">${html}</span>`;
      else if (quoted) out += `<span class="t-id">${html}</span>`;
      else if (number) out += `<span class="t-num">${html}</span>`;
      else if (variable) out += `<span class="t-var">${html}</span>`;
      else if (word) {
        const upper = word.toUpperCase();
        if (KEYWORDS.has(upper)) out += `<span class="t-kw">${html}</span>`;
        else if (TYPES.has(upper)) out += `<span class="t-type">${html}</span>`;
        else if (sql[TOKEN.lastIndex] === "(") out += `<span class="t-fn">${html}</span>`;
        else out += html;
      } else out += html;
    }
    return out;
  }

  /** A name as SQL: bare if Snowflake would read it back unchanged, else quoted. */
  function ident(name) {
    if (/^[A-Z_][A-Z0-9_$]*$/.test(name) && !KEYWORDS.has(name)) return name;
    return `"${name.replace(/"/g, '""')}"`;
  }
  const qualified = (...parts) => parts.filter((p) => p != null).map(ident).join(".");

  // ---------------------------------------------------------------------------
  // Data grid: a virtualized table of a result's rows

  class Grid {
    constructor({ onLoadMore } = {}) {
      this.el = h("div.grid", { tabindex: "0" });
      this.head = h("div.grid-head");
      this.body = h("div.grid-rows");
      this.more = h("div.grid-more");
      this.el.append(this.head, this.body, this.more);
      this.onLoadMore = onLoadMore;
      this.el.addEventListener("scroll", () => {
        this.renderRows();
        this.maybeLoadMore();
      });
      this.el.addEventListener("keydown", (e) => this.onKey(e));
      new ResizeObserver(() => this.renderRows()).observe(this.el);
    }

    set({ columns, rows, total, timezone }) {
      this.columns = columns;
      this.rows = rows;
      this.total = total ?? rows.length;
      this.timezone = timezone;
      this.display = new Array(rows.length);
      this.sort = null;
      this.filter = "";
      this.selected = null;
      this.widths = this.measure();
      this.updateView();
      this.renderHead();
      this.el.scrollTop = 0;
      this.renderRows(true);
    }

    append(rows) {
      this.rows.push(...rows);
      this.display.length = this.rows.length;
      this.updateView();
      this.renderRows(true);
    }

    /** Display text for a row (cached), with nulls as null. */
    rowText(index) {
      let text = this.display[index];
      if (!text) {
        text = this.rows[index].map((v, c) => formatValue(v, this.columns[c], this.timezone));
        this.display[index] = text;
      }
      return text;
    }

    measure() {
      const sample = Math.min(this.rows.length, 200);
      const widths = this.columns.map((column) => Math.max(column.name.length, typeLabel(column).length * 0.85) * 7.4 + 22);
      for (let r = 0; r < sample; r++) {
        this.rowText(r).forEach((text, c) => {
          const length = text == null ? 4 : Math.min(text.length, 60);
          widths[c] = Math.max(widths[c], length * 7.3 + 18);
        });
      }
      return widths.map((w) => Math.round(Math.min(Math.max(w, 64), 420)));
    }

    updateView() {
      let view = this.rows.map((_, i) => i);
      if (this.filter) {
        const needle = this.filter.toLowerCase();
        view = view.filter((i) => this.rowText(i).some((t) => t != null && t.toLowerCase().includes(needle)));
      }
      if (this.sort) {
        const { column, direction } = this.sort;
        const meta = this.columns[column];
        view.sort((a, b) => direction * compareValues(this.rows[a][column], this.rows[b][column], meta) || a - b);
      }
      this.view = view;
      this.body.style.height = view.length * ROW_H + "px";
      this.renderFooter();
    }

    renderFooter() {
      const loaded = this.rows.length;
      let text = "";
      if (this.filter) text = `${plural(this.view.length, "row")} match the filter` + (loaded < this.total ? ` (of ${loaded.toLocaleString()} loaded)` : "");
      else if (loaded < this.total) text = `Showing ${loaded.toLocaleString()} of ${this.total.toLocaleString()} rows. Scroll to load more.`;
      this.more.textContent = text;
      this.more.hidden = !text;
    }

    renderHead() {
      const rnWidth = Math.max(String(this.total).length * 8 + 18, 40);
      this.rnWidth = rnWidth;
      clear(this.head, h("div.hcell.rn", { style: { width: rnWidth + "px" } }, ""));
      this.columns.forEach((column, c) => {
        const sorted = this.sort?.column === c ? (this.sort.direction > 0 ? " ▲" : " ▼") : "";
        const grip = h("span.grip", { onMousedown: (e) => this.startResize(e, c), onClick: (e) => e.stopPropagation() });
        this.head.append(
          h("div.hcell", { style: { width: this.widths[c] + "px" }, title: `${column.name}\n${typeLabel(column)}${column.nullable ? "" : " NOT NULL"}\nClick to sort`, onClick: () => this.toggleSort(c) },
            h("span.hname", null, column.name, sorted),
            h("span.htype", null, typeLabel(column)),
            grip),
        );
      });
    }

    startResize(event, c) {
      event.preventDefault();
      event.stopPropagation();
      const startX = event.clientX;
      const start = this.widths[c];
      const move = (e) => {
        this.widths[c] = Math.max(48, start + e.clientX - startX);
        this.head.children[c + 1].style.width = this.widths[c] + "px";
        this.renderRows(true);
      };
      const up = () => {
        removeEventListener("mousemove", move);
        removeEventListener("mouseup", up);
      };
      addEventListener("mousemove", move);
      addEventListener("mouseup", up);
    }

    async toggleSort(c) {
      if (this.rows.length < this.total && this.onLoadMore) {
        if (this.total <= 200000) await this.onLoadMore(true);
        else toast(`Sorting the ${this.rows.length.toLocaleString()} rows loaded so far`);
      }
      const current = this.sort?.column === c ? this.sort.direction : 0;
      this.sort = current === 0 ? { column: c, direction: 1 } : current === 1 ? { column: c, direction: -1 } : null;
      this.updateView();
      this.renderHead();
      this.renderRows(true);
    }

    setFilter(text) {
      this.filter = text.trim();
      this.selected = null;
      this.updateView();
      this.renderRows(true);
    }

    renderRows(force = false) {
      if (!this.columns) return;
      const headHeight = this.head.offsetHeight || 40;
      const top = Math.max(0, this.el.scrollTop - headHeight);
      const first = Math.max(0, Math.floor(top / ROW_H) - 8);
      const last = Math.min(this.view.length, Math.ceil((top + this.el.clientHeight) / ROW_H) + 8);
      if (!force && first === this.first && last === this.last) return;
      this.first = first;
      this.last = last;
      const fragment = document.createDocumentFragment();
      for (let v = first; v < last; v++) {
        const index = this.view[v];
        const text = this.rowText(index);
        const raw = this.rows[index];
        const row = h("div.grid-row", { class: v % 2 ? "odd" : "", style: { top: v * ROW_H + "px" } },
          h("div.cell.rn", { style: { width: this.rnWidth + "px" } }, index + 1));
        text.forEach((value, c) => {
          const column = this.columns[c];
          let cls = "cell";
          let shown = value;
          if (value == null) {
            cls += " null";
            shown = "NULL";
          } else if (isNumeric(column)) cls += " num";
          else if (column.type === "boolean") cls += raw[c] === "1" ? " bool-t" : " bool-f";
          else if (isJson(column)) {
            cls += " json";
            shown = value.replace(/\n\s*/g, " ");
          } else if (value.includes("\n")) shown = value.replace(/\r?\n/g, "↵ ");
          if (this.selected && this.selected.row === index && this.selected.column === c) cls += " sel";
          row.append(h("div", { class: cls, style: { width: this.widths[c] + "px" }, "data-r": index, "data-c": c }, shown.length > 500 ? shown.slice(0, 500) + "…" : shown));
        });
        fragment.append(row);
      }
      this.body.replaceChildren(fragment);
      if (!this.wired) {
        this.wired = true;
        this.body.addEventListener("click", (e) => this.select(e));
        this.body.addEventListener("dblclick", (e) => {
          const cell = this.cellAt(e);
          if (cell) this.open(cell.row, cell.column);
        });
        this.body.addEventListener("contextmenu", (e) => this.contextMenu(e));
      }
    }

    cellAt(event) {
      const cell = event.target.closest(".cell[data-c]");
      return cell ? { row: Number(cell.dataset.r), column: Number(cell.dataset.c) } : null;
    }

    select(event) {
      const cell = this.cellAt(event);
      if (!cell) return;
      this.selected = cell;
      this.el.focus({ preventScroll: true });
      this.renderRows(true);
    }

    open(row, column) {
      showValue(this.columns[column], this.rows[row][column], this.rowText(row)[column]);
    }

    contextMenu(event) {
      const cell = this.cellAt(event);
      if (!cell) return;
      event.preventDefault();
      this.selected = cell;
      this.renderRows(true);
      const text = this.rowText(cell.row);
      const column = this.columns[cell.column];
      const items = [
        { label: "Copy value", icon: "copy", action: () => copyText(text[cell.column] ?? "", "Value copied") },
        { label: "View value", icon: "view", action: () => this.open(cell.row, cell.column) },
        null,
        { label: "Copy row as TSV", icon: "copy", action: () => copyText(text.map((t) => t ?? "").join("\t"), "Row copied") },
        { label: "Copy row as JSON", icon: "copy", action: () => copyText(JSON.stringify(this.rowObject(cell.row), null, 2), "Row copied") },
        { label: "Copy column name", icon: "column", action: () => copyText(column.name, "Column name copied") },
        null,
        { label: `Filter rows to this value`, icon: "filter", action: () => this.onFilterRequest?.(text[cell.column] ?? "NULL") },
        ...(this.extraCellActions?.(cell, text) || []),
      ];
      showMenu(event.clientX, event.clientY, items);
    }

    rowObject(index) {
      const text = this.rowText(index);
      const object = {};
      this.columns.forEach((column, c) => {
        let value = text[c];
        if (value != null && isJson(column)) {
          try {
            value = JSON.parse(value);
          } catch {}
        } else if (value != null && isNumeric(column) && Number.isSafeInteger(Number(value))) value = Number(value);
        else if (column.type === "boolean" && value != null) value = value === "TRUE";
        object[column.name] = value;
      });
      return object;
    }

    onKey(event) {
      if (!this.selected) return;
      const mod = event.metaKey || event.ctrlKey;
      if (mod && event.key === "c") {
        event.preventDefault();
        copyText(this.rowText(this.selected.row)[this.selected.column] ?? "", "Value copied");
        return;
      }
      if (event.key === "Enter" || event.key === " ") {
        event.preventDefault();
        this.open(this.selected.row, this.selected.column);
        return;
      }
      const moves = { ArrowUp: [-1, 0], ArrowDown: [1, 0], ArrowLeft: [0, -1], ArrowRight: [0, 1] };
      const move = moves[event.key];
      if (!move) return;
      event.preventDefault();
      const position = this.view.indexOf(this.selected.row);
      const v = Math.min(Math.max(position + move[0], 0), this.view.length - 1);
      const c = Math.min(Math.max(this.selected.column + move[1], 0), this.columns.length - 1);
      this.selected = { row: this.view[v], column: c };
      const headHeight = this.head.offsetHeight;
      const rowTop = v * ROW_H + headHeight;
      if (rowTop - headHeight < this.el.scrollTop) this.el.scrollTop = rowTop - headHeight;
      else if (rowTop + ROW_H > this.el.scrollTop + this.el.clientHeight) this.el.scrollTop = rowTop + ROW_H - this.el.clientHeight;
      this.renderRows(true);
    }

    maybeLoadMore() {
      if (!this.onLoadMore || this.loading || this.rows.length >= this.total || this.filter || this.sort) return;
      if (this.el.scrollTop + this.el.clientHeight > this.el.scrollHeight - ROW_H * 40) this.onLoadMore(false);
    }

    /** All loaded rows as text, for export. */
    exportRows() {
      return this.view.map((i) => this.rowText(i));
    }
  }

  /** Values as CSV (RFC 4180) or TSV. */
  function delimited(columns, rows, separator) {
    const quote = (value) => {
      if (value == null) return "";
      if (separator === "\t") return value.replace(/[\t\n\r]/g, " ");
      return /[",\n\r]/.test(value) || value.trim() !== value ? `"${value.replace(/"/g, '""')}"` : value;
    };
    return [columns.map((c) => quote(c.name)), ...rows.map((row) => row.map(quote))].map((r) => r.join(separator)).join("\n") + "\n";
  }

  // ---------------------------------------------------------------------------
  // Catalog: databases, schemas and objects, loaded lazily for the explorer and autocomplete

  const catalog = {
    databases: null,
    byDatabase: new Map(),
    pending: new Map(),
    listeners: new Set(),

    async loadDatabases(force = false) {
      if (this.databases && !force) return this.databases;
      const data = await api("/catalog");
      if (data.error) throw new Error(data.error.message);
      this.databases = data.databases;
      for (const name of this.byDatabase.keys()) if (!this.databases.some((d) => d.name === name)) this.byDatabase.delete(name);
      this.changed();
      return this.databases;
    },

    async load(database, force = false) {
      if (!force && this.byDatabase.has(database)) return this.byDatabase.get(database);
      if (!force && this.pending.has(database)) return this.pending.get(database);
      const promise = api("/catalog/" + encodeURIComponent(database)).then((data) => {
        if (data.error) throw new Error(data.error.message);
        this.byDatabase.set(database, data);
        this.changed();
        return data;
      }).finally(() => this.pending.delete(database));
      this.pending.set(database, promise);
      return promise;
    },

    /** Reloads the database list and every database loaded so far. */
    async refresh() {
      await this.loadDatabases(true);
      await Promise.all([...this.byDatabase.keys()].map((db) => this.load(db, true).catch(() => this.byDatabase.delete(db))));
    },

    schemas(database) {
      return this.byDatabase.get(database)?.schemas || [];
    },

    object(database, schema, name) {
      return this.schemas(database).find((s) => s.name === schema)?.objects.find((o) => o.name === name);
    },

    changed() {
      for (const listener of this.listeners) listener();
    },
  };

  // ---------------------------------------------------------------------------
  // Editor: a textarea over a highlighted copy of its text

  class Editor {
    constructor({ value, onChange, onKeyCommand, onCaret }) {
      this.textarea = h("textarea", { spellcheck: "false", autocapitalize: "off", autocomplete: "off", wrap: "off", placeholder: `Write SQL here.  ${MOD}+Enter runs the statement at the cursor, ${MOD}+Shift+Enter runs everything.` });
      this.textarea.value = value;
      this.marks = h("pre.marks", { "aria-hidden": "true" });
      this.pre = h("pre", { "aria-hidden": "true" });
      this.gutter = h("div.gutter");
      this.el = h("div.editor", null, this.gutter, h("div.code", null, this.marks, this.pre, this.textarea));
      this.onChange = onChange;
      this.onKeyCommand = onKeyCommand;
      this.onCaret = onCaret;
      this.statement = null;
      this.completion = null;

      this.textarea.addEventListener("input", (e) => {
        this.render();
        this.onChange(this.value);
        if (!this.editing && e.inputType === "insertText" && /^[\w$.]$/.test(e.data || "")) this.complete(false);
        else this.closeCompletion();
      });
      this.textarea.addEventListener("scroll", () => this.syncScroll());
      this.textarea.addEventListener("keydown", (e) => this.onKey(e));
      this.textarea.addEventListener("blur", () => setTimeout(() => this.closeCompletion(), 150));
      const caret = () => {
        this.renderGutter();
        this.onCaret?.();
      };
      this.textarea.addEventListener("keyup", caret);
      this.textarea.addEventListener("click", caret);
      document.addEventListener("selectionchange", () => {
        if (document.activeElement === this.textarea) caret();
      });
      this.render();
    }

    get value() {
      return this.textarea.value;
    }

    focus() {
      this.textarea.focus({ preventScroll: true });
    }

    render() {
      const text = this.value;
      this.pre.innerHTML = highlight(text) + "\n ";
      this.renderMarks();
      this.renderGutter();
      this.syncScroll();
    }

    /** Marks the statement the cursor is in. */
    markStatement(range) {
      const same = range && this.statement && range[0] === this.statement[0] && range[1] === this.statement[1];
      if (same || (!range && !this.statement)) return;
      this.statement = range;
      this.renderMarks();
    }

    renderMarks() {
      const text = this.value;
      const range = this.statement;
      if (!range || range[1] > text.length || this.textarea.selectionStart !== this.textarea.selectionEnd) {
        this.marks.textContent = "";
        return;
      }
      this.marks.innerHTML = escapeHtml(text.slice(0, range[0])) + "<mark>" + escapeHtml(text.slice(range[0], range[1])) + "</mark>" + escapeHtml(text.slice(range[1])) + "\n ";
    }

    renderGutter() {
      const text = this.value;
      let lines = 1;
      for (let i = 0; i < text.length; i++) if (text.charCodeAt(i) === 10) lines++;
      const current = text.slice(0, this.textarea.selectionStart).split("\n").length;
      if (lines !== this.lines || current !== this.currentLine) {
        this.lines = lines;
        this.currentLine = current;
        const numbers = [];
        for (let i = 1; i <= lines; i++) numbers.push(i === current ? `<span class="cur">${i}</span>` : String(i));
        this.gutter.innerHTML = numbers.join("\n") + "\n ";
      }
      this.syncScroll();
    }

    syncScroll() {
      const { scrollTop, scrollLeft } = this.textarea;
      this.pre.scrollTop = this.marks.scrollTop = this.gutter.scrollTop = scrollTop;
      this.pre.scrollLeft = this.marks.scrollLeft = scrollLeft;
    }

    selection() {
      const { selectionStart, selectionEnd } = this.textarea;
      return { start: selectionStart, end: selectionEnd, text: this.value.slice(selectionStart, selectionEnd) };
    }

    /** Replaces a range, keeping the browser's undo history. */
    replace(start, end, text, selectStart = start + text.length, selectEnd = selectStart) {
      const ta = this.textarea;
      ta.focus();
      ta.setSelectionRange(start, end);
      this.editing = true;
      try {
        const done = text === "" ? document.execCommand("delete") : document.execCommand("insertText", false, text);
        if (!done) {
          ta.setRangeText(text, start, end, "end");
          ta.dispatchEvent(new Event("input"));
        }
      } finally {
        this.editing = false;
      }
      ta.setSelectionRange(selectStart, selectEnd);
      this.render();
    }

    insert(text) {
      const { start, end } = this.selection();
      this.replace(start, end, text);
    }

    setValue(text) {
      this.replace(0, this.value.length, text, 0, 0);
      this.textarea.scrollTop = 0;
    }

    onKey(event) {
      const mod = event.metaKey || event.ctrlKey;
      if (this.completion && this.completionKey(event)) return;
      if (mod && event.key === "Enter") {
        event.preventDefault();
        this.onKeyCommand(event.shiftKey ? "runAll" : "run");
      } else if (mod && (event.key === "s" || event.key === "S")) {
        event.preventDefault();
        this.onKeyCommand("save");
      } else if (mod && event.key === "/") {
        event.preventDefault();
        this.toggleComment();
      } else if (event.ctrlKey && event.code === "Space") {
        event.preventDefault();
        this.complete(true);
      } else if (event.key === "Tab" && !mod && !event.altKey) {
        event.preventDefault();
        this.indent(event.shiftKey);
      } else if (event.key === "Enter" && !event.shiftKey && !event.altKey) {
        event.preventDefault();
        const { start, end } = this.selection();
        const lineStart = this.value.lastIndexOf("\n", start - 1) + 1;
        const indentation = /^[ \t]*/.exec(this.value.slice(lineStart, start))[0];
        this.replace(start, end, "\n" + indentation);
      }
    }

    /** The lines a selection touches: [start of first line, end of last line]. */
    selectedLines() {
      const { start, end } = this.selection();
      const text = this.value;
      const from = text.lastIndexOf("\n", start - 1) + 1;
      let to = text.indexOf("\n", end > start && text[end - 1] === "\n" ? end - 1 : end);
      if (to === -1) to = text.length;
      return [from, to];
    }

    indent(outdent) {
      const { start, end } = this.selection();
      const text = this.value;
      if (!outdent && !text.slice(start, end).includes("\n")) {
        this.replace(start, end, "  ");
        return;
      }
      const [from, to] = this.selectedLines();
      const lines = text.slice(from, to).split("\n");
      const changed = lines.map((line) => (outdent ? line.replace(/^( {1,2}|\t)/, "") : "  " + line));
      const result = changed.join("\n");
      this.replace(from, to, result, from, from + result.length);
    }

    toggleComment() {
      const [from, to] = this.selectedLines();
      const lines = this.value.slice(from, to).split("\n");
      const commented = lines.filter((l) => l.trim()).every((l) => /^\s*--/.test(l));
      const changed = lines.map((line) => {
        if (!line.trim()) return line;
        return commented ? line.replace(/^(\s*)-- ?/, "$1") : line.replace(/^(\s*)/, "$1-- ");
      });
      const result = changed.join("\n");
      this.replace(from, to, result, from, from + result.length);
    }

    // Autocomplete -------------------------------------------------------------

    /** The identifier path being typed before the cursor: `["DB", "SCH", "pre"]`. */
    wordAtCursor() {
      const { start } = this.selection();
      const before = this.value.slice(0, start);
      const match = /((?:(?:"[^"]*"|[A-Za-z_][\w$]*)\.)*)("?[\w$]*)$/.exec(before);
      const path = match[1]
        ? match[1].slice(0, -1).match(/"[^"]*"|[A-Za-z_][\w$]*/g).map((p) => (p.startsWith('"') ? p.slice(1, -1) : p.toUpperCase()))
        : [];
      return { path, prefix: match[2], start: start - match[2].length, end: start };
    }

    async complete(explicit) {
      const word = this.wordAtCursor();
      if (!explicit && !word.path.length && word.prefix.length < 2) return this.closeCompletion();
      const items = await completions(word, this.value, this.selection().start);
      const current = this.wordAtCursor();
      if (current.start !== word.start || current.prefix !== word.prefix) return;
      const prefix = word.prefix.replace(/^"/, "").toUpperCase();
      const matches = items
        .filter((item) => item.label.toUpperCase().startsWith(prefix))
        .sort((a, b) => a.rank - b.rank || a.label.localeCompare(b.label))
        .slice(0, 60);
      if (!matches.length || (!explicit && matches.length === 1 && matches[0].label.toUpperCase() === prefix)) return this.closeCompletion();
      this.showCompletion(word, matches);
    }

    showCompletion(word, items) {
      this.closeCompletion();
      const list = h("div.complete");
      items.forEach((item, i) => {
        list.append(h("div", { class: i === 0 ? "active" : "", onMousedown: (e) => { e.preventDefault(); this.accept(i); } },
          icon(item.icon || "chevron"), item.label, h("span.kind", null, item.kind)));
      });
      document.body.append(list);
      const position = this.caretPosition(word.start);
      list.style.left = Math.min(position.x, innerWidth - list.offsetWidth - 8) + "px";
      list.style.top = (position.y + list.offsetHeight > innerHeight - 8 ? position.y - list.offsetHeight - 22 : position.y) + "px";
      this.completion = { list, items, word, active: 0 };
    }

    completionKey(event) {
      const completion = this.completion;
      if (event.key === "ArrowDown" || event.key === "ArrowUp") {
        event.preventDefault();
        const n = completion.items.length;
        completion.active = (completion.active + (event.key === "ArrowDown" ? 1 : n - 1)) % n;
        [...completion.list.children].forEach((el, i) => el.classList.toggle("active", i === completion.active));
        completion.list.children[completion.active].scrollIntoView({ block: "nearest" });
        return true;
      }
      if (event.key === "Enter" || event.key === "Tab") {
        if (event.metaKey || event.ctrlKey) return false;
        event.preventDefault();
        this.accept(completion.active);
        return true;
      }
      if (event.key === "Escape") {
        event.preventDefault();
        this.closeCompletion();
        return true;
      }
      return false;
    }

    accept(index) {
      const { items, word } = this.completion;
      const item = items[index];
      this.closeCompletion();
      this.replace(word.start, this.selection().start, item.insert ?? item.label);
    }

    closeCompletion() {
      this.completion?.list.remove();
      this.completion = null;
    }

    /** Viewport coordinates just below a text offset (the editor never wraps lines). */
    caretPosition(offset) {
      if (!this.charWidth) {
        const probe = h("span", { style: { visibility: "hidden", position: "absolute" } }, "x".repeat(100));
        this.pre.append(probe);
        this.charWidth = probe.getBoundingClientRect().width / 100;
        probe.remove();
      }
      const before = this.value.slice(0, offset);
      const line = before.split("\n").length - 1;
      const column = before.length - before.lastIndexOf("\n") - 1;
      const rect = this.textarea.getBoundingClientRect();
      return {
        x: rect.left + 12 + column * this.charWidth - this.textarea.scrollLeft,
        y: rect.top + 10 + (line + 1) * 20 - this.textarea.scrollTop + 2,
      };
    }
  }

  /** Tables a statement reads, by alias (for completing `alias.`). */
  function tableAliases(sql) {
    const aliases = new Map();
    const pattern = /\b(?:FROM|JOIN)\s+((?:"[^"]+"|[A-Za-z_][\w$]*)(?:\.(?:"[^"]+"|[A-Za-z_][\w$]*)){0,2})(?:\s+(?:AS\s+)?([A-Za-z_][\w$]*))?/gi;
    const reserved = new Set(["WHERE", "JOIN", "ON", "LEFT", "RIGHT", "INNER", "OUTER", "FULL", "CROSS", "GROUP", "ORDER", "LIMIT", "QUALIFY", "HAVING", "UNION", "SAMPLE", "NATURAL", "USING", "MINUS", "EXCEPT", "WINDOW", "LATERAL"]);
    for (const match of sql.matchAll(pattern)) {
      const parts = match[1].match(/"[^"]+"|[A-Za-z_][\w$]*/g).map((p) => (p.startsWith('"') ? p.slice(1, -1) : p.toUpperCase()));
      const alias = match[2] && !reserved.has(match[2].toUpperCase()) ? match[2].toUpperCase() : null;
      aliases.set(parts[parts.length - 1], parts);
      if (alias) aliases.set(alias, parts);
    }
    return aliases;
  }

  /** Completion candidates for an identifier path in the active worksheet's context. */
  async function completions(word, sql, offset) {
    const ws = worksheets.active();
    const database = ws?.model.database;
    const schema = ws?.model.schema;
    await catalog.loadDatabases().catch(() => []);
    if (database) await catalog.load(database).catch(() => null);
    const items = [];
    const add = (label, kind, iconName, rank) => items.push({ label, insert: ident(label), kind, icon: iconName, rank });
    const objectItems = (db, sch) => {
      for (const o of catalog.schemas(db).find((s) => s.name === sch)?.objects || []) add(o.name, o.kind.replace("_", " ").toLowerCase(), o.kind.includes("VIEW") ? "view" : "table", 1);
    };
    const columnItems = (object) => object?.columns.forEach((c) => add(c.name, c.type, "column", 0));
    const resolveObject = (parts) => {
      const [db, sch, name] = parts.length === 3 ? parts : parts.length === 2 ? [database, ...parts] : [database, schema, parts[0]];
      if (db && !catalog.byDatabase.has(db)) return null;
      return catalog.object(db, sch, name);
    };

    const path = word.path;
    if (path.length) {
      // `alias.` or `table.` → columns; `db.` → schemas; `db.schema.` / `schema.` → objects.
      const statement = sql.slice(Math.max(0, sql.lastIndexOf(";", offset - 1) + 1), offset + 2000);
      const aliases = tableAliases(statement);
      const last = path[path.length - 1];
      if (path.length === 1 && aliases.has(last)) {
        const parts = aliases.get(last);
        const db = parts.length === 3 ? parts[0] : database;
        if (db) await catalog.load(db).catch(() => null);
        columnItems(resolveObject(parts));
      }
      if (path.length === 1 && catalog.databases?.some((d) => d.name === last)) {
        const data = await catalog.load(last).catch(() => null);
        data?.schemas.forEach((s) => add(s.name, "schema", "schema", 1));
      }
      if (path.length === 1 && database) objectItems(database, last);
      if (path.length === 1 && database && schema) columnItems(catalog.object(database, schema, last));
      if (path.length === 2) {
        if (catalog.databases?.some((d) => d.name === path[0])) {
          await catalog.load(path[0]).catch(() => null);
          objectItems(path[0], path[1]);
        }
        if (database) columnItems(catalog.object(database, path[0], path[1]));
      }
      if (path.length === 3) {
        await catalog.load(path[0]).catch(() => null);
        columnItems(catalog.object(path[0], path[1], path[2]));
      }
      return items;
    }

    // Columns of the tables in this statement, then objects, schemas, databases, keywords and functions.
    const statement = sql.slice(Math.max(0, sql.lastIndexOf(";", offset - 1) + 1), offset + 2000);
    for (const parts of new Set(tableAliases(statement).values())) columnItems(resolveObject(parts));
    if (database && schema) objectItems(database, schema);
    if (database) catalog.schemas(database).forEach((s) => add(s.name, "schema", "schema", 2));
    catalog.databases?.forEach((d) => add(d.name, "database", "db", 2));
    for (const keyword of KEYWORDS) items.push({ label: keyword, kind: "keyword", icon: "chevron", rank: 3 });
    for (const fn of FUNCTIONS) items.push({ label: fn, insert: fn + "(", kind: "function", icon: "func", rank: 4 });
    const seen = new Set();
    return items.filter((item) => {
      const key = item.label + "|" + item.kind;
      if (seen.has(key)) return false;
      seen.add(key);
      return true;
    });
  }

  // ---------------------------------------------------------------------------
  // Saved queries

  const saved = {
    items: store.get("saved", []),
    listeners: new Set(),
    persist() {
      store.set("saved", this.items);
      for (const listener of this.listeners) listener();
    },
    get(id) {
      return this.items.find((q) => q.id === id);
    },
    upsert(query) {
      const now = Date.now();
      const existing = this.get(query.id);
      if (existing) Object.assign(existing, query, { updatedAt: now });
      else this.items.unshift({ id: uid(), createdAt: now, updatedAt: now, ...query });
      this.persist();
      return existing || this.items[0];
    },
    remove(id) {
      this.items = this.items.filter((q) => q.id !== id);
      this.persist();
    },
  };

  /** Asks for a name and saves the worksheet's SQL as a saved query. */
  async function saveWorksheet(ws, { rename = false } = {}) {
    const existing = ws.model.savedId && saved.get(ws.model.savedId);
    let name = existing?.name;
    if (!existing || rename) {
      const firstComment = /^\s*--\s*(.+)/.exec(ws.editor.value)?.[1];
      name = await promptDialog({
        title: existing ? "Rename saved query" : "Save query",
        label: "Name",
        value: existing?.name || (ws.model.name.startsWith("Worksheet") ? firstComment || "" : ws.model.name),
        placeholder: "e.g. Orders by region",
        help: "Saved queries are kept in this browser.",
        confirmLabel: "Save",
      });
      if (!name) return;
    }
    const query = saved.upsert({ id: existing?.id, name, sql: ws.editor.value, database: ws.model.database, schema: ws.model.schema });
    ws.model.savedId = query.id;
    ws.model.name = name;
    ws.savedSql = query.sql;
    worksheets.persist();
    worksheets.renderTabs();
    toast(`Saved “${name}”`);
  }

  // ---------------------------------------------------------------------------
  // Worksheets

  class Worksheet {
    constructor(model) {
      this.model = model;
      this.session = null;
      this.context = null;
      this.running = null;
      this.results = [];
      this.error = null;
      this.activeResult = 0;
      this.savedSql = model.savedId ? saved.get(model.savedId)?.sql : null;
      this.ranges = null;
      this.rangesFor = null;
      // Reuse this worksheet's session across page reloads.
      this.session = worksheetSessions.get(model.id)?.token || null;

      this.editor = new Editor({
        value: model.sql,
        onChange: (sql) => {
          this.model.sql = sql;
          this.ranges = null;
          worksheets.persistSoon();
          worksheets.renderTabs();
          this.updateStatementMark();
        },
        onKeyCommand: (command) => {
          if (command === "run") this.run("cursor");
          else if (command === "runAll") this.run("all");
          else if (command === "save") saveWorksheet(this);
        },
        onCaret: () => this.updateStatementMark(),
      });
      this.databaseSelect = h("select", { title: "Database", onChange: () => this.useContext(this.databaseSelect.value || null, null) });
      this.schemaSelect = h("select", { title: "Schema", onChange: () => this.useContext(this.model.database, this.schemaSelect.value || null) });
      this.runButton = h("button.btn.primary", { type: "button", title: `Run the statement at the cursor, or the selection (${MOD}+Enter)`, onClick: () => this.run("cursor") }, icon("play"), "Run", h("kbd", null, `${MOD}↵`));
      this.runAllButton = h("button.btn", { type: "button", title: `Run every statement (${MOD}+Shift+Enter)`, onClick: () => this.run("all") }, icon("playAll"), "Run all");
      this.cancelButton = h("button.btn.danger", { type: "button", hidden: true, onClick: () => this.cancel() }, icon("stop"), "Cancel");
      this.saveButton = h("button.btn", { type: "button", title: `Save as a saved query (${MOD}+S)`, onClick: () => saveWorksheet(this) }, icon("save"), "Save");
      this.toolbar = h("div.toolbar", null,
        h("div.ctx", null, icon("db"), this.databaseSelect, h("span.faint", null, "."), this.schemaSelect),
        h("div.spacer"),
        this.saveButton, this.cancelButton, this.runAllButton, this.runButton);
      this.resultsEl = h("div.results");
      const splitter = h("div.splitter-h", { title: "Drag to resize" });
      this.el = h("div.ws-body", null, this.toolbar, this.editor.el, splitter, this.resultsEl);
      this.el.style.setProperty("--editor-h", store.get("editorHeight", 42) + "%");
      dragSplitter(splitter, "y", (delta, start) => {
        const height = this.el.getBoundingClientRect().height;
        const percent = Math.min(85, Math.max(12, ((start + delta) / height) * 100));
        this.el.style.setProperty("--editor-h", percent + "%");
        store.set("editorHeight", Math.round(percent));
      }, () => this.editor.el.getBoundingClientRect().height);
      this.renderContext();
      this.renderResults();
      this.restoreResults();
    }

    get dirty() {
      return this.model.savedId != null && this.savedSql != null && this.savedSql !== this.model.sql;
    }

    // Context -----------------------------------------------------------------

    renderContext() {
      const databases = catalog.databases?.map((d) => d.name) || [];
      const db = this.model.database;
      const options = [...new Set([...databases, ...(db ? [db] : [])])];
      clear(this.databaseSelect, h("option", { value: "" }, "No database"), options.map((name) => h("option", { value: name, selected: name === db }, name)));
      const schemas = db ? catalog.schemas(db).map((s) => s.name) : [];
      const schema = this.model.schema;
      const schemaOptions = [...new Set([...schemas, ...(schema ? [schema] : [])])];
      clear(this.schemaSelect, h("option", { value: "" }, "No schema"), schemaOptions.map((name) => h("option", { value: name, selected: name === schema }, name)));
      this.schemaSelect.disabled = !db;
      if (db && !catalog.byDatabase.has(db)) catalog.load(db).catch(() => {});
    }

    applyContext(context) {
      this.context = context;
      this.model.database = context.database;
      this.model.schema = context.schema;
      worksheets.persistSoon();
      this.renderContext();
    }

    async useContext(database, schema) {
      if (!database) {
        // There's no USE for "no database": start a fresh session without one.
        this.endSession();
        this.model.database = null;
        this.model.schema = null;
        this.renderContext();
        return;
      }
      const sql = schema ? `USE SCHEMA ${qualified(database, schema)}` : `USE DATABASE ${ident(database)}`;
      try {
        const data = await this.execute(sql);
        if (data.error) toast(data.error.message, "error");
        if (data.context) this.applyContext(data.context);
      } catch (err) {
        toast(err.message, "error");
        this.renderContext();
      }
    }

    // Running -----------------------------------------------------------------

    async ensureSession() {
      if (this.session) return this.session;
      const data = await api("/sessions", { method: "POST", body: { database: this.model.database, schema: this.model.schema } });
      this.session = data.session;
      this.context = data.context;
      worksheetSessions.set(this.model.id, { token: data.session, id: data.context.sessionId });
      return this.session;
    }

    /** Ends this worksheet's session, if it has one. */
    endSession() {
      const id = this.context?.sessionId ?? worksheetSessions.get(this.model.id)?.id;
      if (id != null) api(`/sessions/${id}`, { method: "DELETE" }).catch(() => {});
      this.session = null;
      this.context = null;
      worksheetSessions.set(this.model.id, null);
    }

    /** Runs SQL in this worksheet's session, starting a new one if it ended. */
    async execute(sql, requestId) {
      for (let attempt = 0; ; attempt++) {
        const session = await this.ensureSession();
        try {
          return await api("/query", { method: "POST", body: { session, sql, requestId } });
        } catch (err) {
          if (err.code === "390111" && attempt === 0) {
            this.session = null;
            this.context = null;
            continue;
          }
          throw err;
        }
      }
    }

    /** Where each statement is, or an error if the SQL can't be split (an unclosed quote, say). */
    async statementRanges() {
      const sql = this.editor.value;
      if (this.ranges && this.rangesFor === sql) return this.ranges;
      const data = await api("/split", { method: "POST", body: { sql } });
      if (data.error && !data.ranges.length) throw new Error(data.error);
      this.ranges = data.ranges;
      this.rangesFor = sql;
      return this.ranges;
    }

    /** The statement at an offset: the one containing it, else the nearest one before it. */
    static statementAt(ranges, offset) {
      let best = null;
      for (const range of ranges) {
        if (offset >= range[0] && offset <= range[1] + 1) return range;
        if (range[1] < offset) best = range;
      }
      return best || ranges[0] || null;
    }

    updateStatementMark = debounce(async () => {
      if (!this.editor.value.trim() || document.activeElement !== this.editor.textarea) return this.editor.markStatement(null);
      try {
        const ranges = await this.statementRanges();
        this.editor.markStatement(ranges.length > 1 ? Worksheet.statementAt(ranges, this.editor.selection().start) : null);
      } catch {
        this.editor.markStatement(null);
      }
    }, 120);

    async run(mode) {
      if (this.running) return;
      const { text: selected } = this.editor.selection();
      let sql = this.editor.value;
      if (selected.trim()) sql = selected;
      else if (mode === "cursor") {
        try {
          const ranges = await this.statementRanges();
          const range = Worksheet.statementAt(ranges, this.editor.selection().start);
          if (range) sql = this.editor.value.slice(range[0], range[1]);
        } catch (err) {
          // Running everything instead could run statements the cursor isn't on.
          if (!/empty/i.test(err.message)) return toast(`Can't find the statement at the cursor: ${err.message}. Select the SQL to run it.`, "error");
        }
      }
      if (!sql.trim()) return toast("Nothing to run");
      await this.runSql(sql);
    }

    /** Runs SQL and shows its results (the explorer's actions use this too). */
    async runSql(sql, { label } = {}) {
      if (this.running) {
        toast("A query is already running in this worksheet", "error");
        return null;
      }
      const requestId = uid();
      this.running = { requestId, started: performance.now(), sql, label };
      this.renderRunning();
      worksheets.renderTabs();
      let data = null;
      try {
        data = await this.execute(sql, requestId);
        this.results = data.results.map((r) => ({ ...r }));
        this.error = data.error;
        if (data.context) this.applyContext(data.context);
      } catch (err) {
        this.results = [];
        this.error = { message: err.message };
      } finally {
        this.running = null;
        clearInterval(this.timer);
      }
      this.activeResult = this.error ? this.results.length : Math.max(0, this.results.length - 1);
      this.model.lastQueryIds = this.results.map((r) => r.queryId);
      this.model.lastError = this.error;
      worksheets.persistSoon();
      this.renderResults();
      worksheets.renderTabs();
      if (this.results.some((r) => !["query", "show"].includes(r.kind))) refreshCatalogSoon();
      return data;
    }

    async cancel() {
      if (!this.running || !this.session) return;
      try {
        await api("/cancel", { method: "POST", body: { session: this.session, requestId: this.running.requestId } });
      } catch (err) {
        toast(err.message, "error");
      }
    }

    /** Shows the last results after a reload, if localflake still has them. */
    async restoreResults() {
      const ids = this.model.lastQueryIds || [];
      if (!ids.length && !this.model.lastError) return;
      const results = [];
      for (const id of ids) {
        try {
          const data = await api(`/results/${encodeURIComponent(id)}`);
          if (data.error) break;
          results.push(data);
        } catch {
          break;
        }
      }
      if (this.running || this.results.length) return;
      this.results = results;
      this.error = results.length === ids.length ? this.model.lastError : null;
      this.activeResult = this.error ? results.length : Math.max(0, results.length - 1);
      this.renderResults();
    }

    // Results -----------------------------------------------------------------

    renderRunning() {
      const elapsed = h("b", null, "0.0 s");
      clear(this.resultsEl, h("div.running", null,
        h("span.spinner.big"),
        h("div", null, this.running.label || "Running…", " ", elapsed),
        h("button.btn.danger", { type: "button", onClick: () => this.cancel() }, icon("stop"), "Cancel")));
      clearInterval(this.timer);
      this.timer = setInterval(() => {
        if (this.running) elapsed.textContent = ((performance.now() - this.running.started) / 1000).toFixed(1) + " s";
      }, 100);
      this.runButton.disabled = this.runAllButton.disabled = true;
      this.cancelButton.hidden = false;
    }

    renderResults() {
      this.runButton.disabled = this.runAllButton.disabled = false;
      this.cancelButton.hidden = true;
      const entries = [...this.results];
      if (this.error) entries.push({ error: this.error });
      if (!entries.length) {
        clear(this.resultsEl, h("div.res-placeholder", null,
          h("div", null, "Results appear here."),
          h("div.hint", null, `${MOD}+Enter runs the statement at the cursor (or the selection), ${MOD}+Shift+Enter runs them all.`),
          h("div.hint", null, "Every statement any client runs is in the Query history.")));
        return;
      }
      const index = Math.min(this.activeResult, entries.length - 1);
      const entry = entries[index];
      const tabs = entries.length > 1
        ? h("div.res-tabs", null, entries.map((e, i) => h("button", {
            type: "button",
            class: i === index ? "active" : "",
            title: e.statement || e.error?.message || "",
            onClick: () => { this.activeResult = i; this.renderResults(); },
          }, e.error ? h("span.err", null, "✕ Error") : `${i + 1}. ${resultSummary(e)}`)))
        : null;
      clear(this.resultsEl, tabs, entry.error ? errorView(entry.error) : this.resultView(entry));
    }

    resultView(result) {
      if (result.transfer) {
        return h("div.res-message", null,
          "PUT and GET move files between a driver and a stage, so they can't run in the browser. ",
          "To upload a file, use “Upload file…” on a stage in the explorer, or “Load data…” on a table.");
      }
      return resultPanel(result);
    }
  }

  function resultSummary(result) {
    if (result.kind === "query" || result.kind === "show") return plural(result.total, "row");
    const first = result.columns[0]?.name || "";
    if (result.kind === "insert" || result.kind === "update" || result.kind === "delete" || result.kind === "merge") {
      return `${result.kind} ${result.rows[0]?.[0] ?? ""}`.trim();
    }
    if (first === "status") {
      const status = String(result.rows[0]?.[0] ?? result.kind);
      return status.length > 40 ? status.slice(0, 39) + "…" : status;
    }
    return plural(result.total, "row");
  }

  function formatDuration(micros) {
    if (micros == null) return "";
    if (micros < 1000) return `${micros} µs`;
    if (micros < 1e6) return `${(micros / 1000).toFixed(micros < 1e4 ? 1 : 0)} ms`;
    return `${(micros / 1e6).toFixed(2)} s`;
  }

  function errorView(error) {
    return h("div.res-error", null,
      h("div.codes", null,
        h("b", null, "Error"),
        error.code ? h("span", null, "Code ", h("b.mono", null, error.code)) : null,
        error.sqlState ? h("span", null, "SQLSTATE ", h("b.mono", null, error.sqlState)) : null,
        error.queryId ? h("span", null, "Query ID ", h("span.mono", null, error.queryId), " ",
          h("a", { href: "#", onClick: (e) => { e.preventDefault(); history.open(error.queryId); } }, "details")) : null),
      h("pre", null, error.message));
  }

  /** A result's grid with its toolbar: counts, timing, filter, copy and download. */
  function resultPanel(result, { compact = false } = {}) {
    const grid = new Grid({
      onLoadMore: async (all) => {
        if (grid.loading) return grid.loading;
        grid.loading = (async () => {
          try {
            while (grid.rows.length < grid.total) {
              const limit = all ? 100000 : 5000;
              const data = await api(`/results/${encodeURIComponent(result.queryId)}?offset=${grid.rows.length}&limit=${limit}`);
              if (data.error) throw new Error(data.error.message);
              if (!data.rows.length) break;
              grid.append(data.rows);
              result.rows = grid.rows;
              if (!all) break;
            }
          } catch (err) {
            toast(err.message, "error");
          } finally {
            grid.loading = null;
            update();
          }
        })();
        return grid.loading;
      },
    });
    grid.set({ columns: result.columns, rows: result.rows, total: result.total, timezone: result.timezone });

    const filter = h("input", { type: "search", placeholder: "Filter rows", onInput: () => { grid.setFilter(filter.value); update(); } });
    grid.onFilterRequest = (value) => {
      filter.value = value;
      grid.setFilter(value);
      update();
    };
    const listing = /^\s*(?:LIST|LS)\s+(@\S+)/i.exec(result.statement || "");
    if (listing) {
      grid.extraCellActions = (cell, text) => {
        const nameColumn = result.columns.findIndex((c) => c.name === "name");
        if (nameColumn < 0) return [];
        return [null, { label: "Download file", icon: "download", action: () => downloadStageFile(listing[1], text[nameColumn]) }];
      };
    }
    const count = h("span.stat");
    const update = () => {
      const loaded = grid.rows.length;
      clear(count, h("span.ok", null, "✓"), h("b", null, plural(result.total, "row")),
        loaded < result.total ? h("span.faint", null, `(${loaded.toLocaleString()} loaded)`) : null);
    };
    update();

    const allRows = async () => {
      if (grid.rows.length < grid.total) await grid.onLoadMore(true);
      return grid.exportRows();
    };
    const exportMenu = (e) => {
      const name = (result.statement || "result").match(/from\s+([\w."$]+)/i)?.[1]?.replace(/["$]/g, "").toLowerCase() || "result";
      showMenu(e.clientX, e.clientY, [
        { label: "Copy as TSV (for spreadsheets)", icon: "copy", action: async () => copyText(delimited(result.columns, await allRows(), "\t"), "Rows copied") },
        { label: "Copy as JSON", icon: "copy", action: async () => { await allRows(); copyText(JSON.stringify(grid.view.map((i) => grid.rowObject(i)), null, 2), "Rows copied"); } },
        null,
        { label: "Download CSV", icon: "download", action: async () => download(`${name}.csv`, delimited(result.columns, await allRows(), ","), "text/csv") },
        { label: "Download JSON", icon: "download", action: async () => { await allRows(); download(`${name}.json`, JSON.stringify(grid.view.map((i) => grid.rowObject(i)), null, 2), "application/json"); } },
      ]);
    };

    const bar = h("div.res-bar", null,
      count,
      result.durationMicros != null ? h("span.stat", { title: "Execution time" }, formatDuration(result.durationMicros)) : null,
      compact ? null : h("span.stat.mono", { title: "Query ID: click to copy", style: { cursor: "pointer" }, onClick: () => copyText(result.queryId, "Query ID copied") }, icon("hash"), result.queryId.slice(0, 13) + "…"),
      compact ? null : h("span.stmt", { title: result.statement }, result.statement),
      compact ? h("div.spacer") : null,
      filter,
      h("button.btn.small", { type: "button", onClick: exportMenu }, icon("download"), "Export"),
    );
    return h("div.res-body", null, bar, grid.el);
  }

  async function downloadStageFile(stage, listedName) {
    const ws = worksheets.active();
    // LIST shows named stages' files under the stage's lower-case name: drop it.
    let name = listedName;
    const stageName = /^@(?:[^/]*\.)?("?)([^/".]+)\1/.exec(stage)?.[2];
    if (stageName && !stage.startsWith("@~") && !stage.startsWith("@%") && name.toLowerCase().startsWith(stageName.toLowerCase() + "/")) {
      name = name.slice(stageName.length + 1);
    }
    try {
      const session = await ws.ensureSession();
      const url = `/__localflake/api/stage-files?session=${encodeURIComponent(session)}&stage=${encodeURIComponent(stage.split("/")[0])}&name=${encodeURIComponent(name)}`;
      const response = await fetch(url);
      if (!response.ok) {
        const data = await response.json().catch(() => ({}));
        throw new Error(data.error?.message || response.statusText);
      }
      const blob = await response.blob();
      const a = h("a", { href: URL.createObjectURL(blob), download: name.split("/").pop() });
      document.body.append(a);
      a.click();
      a.remove();
    } catch (err) {
      toast(err.message, "error");
    }
  }

  const worksheets = {
    list: [],
    activeId: null,
    tabsEl: h("div.ws-tabs"),
    bodyEl: h("div.ws-holder", { style: { display: "flex", flexDirection: "column", flex: "1", minHeight: "0" } }),

    init() {
      const state = store.get("worksheets", null);
      const models = state?.tabs?.length ? state.tabs : [{ id: uid(), name: "Worksheet 1", sql: STARTER_SQL, database: null, schema: null }];
      this.list = models.map((m) => new Worksheet(m));
      this.activate(state?.active && this.list.some((w) => w.model.id === state.active) ? state.active : this.list[0].model.id);
    },

    active() {
      return this.list.find((w) => w.model.id === this.activeId);
    },

    persist() {
      store.set("worksheets", { tabs: this.list.map((w) => w.model), active: this.activeId });
    },
    persistSoon: debounce(() => worksheets.persist(), 400),

    activate(id) {
      this.activeId = id;
      const ws = this.active();
      clear(this.bodyEl, ws.el);
      this.renderTabs();
      this.persist();
      ws.renderContext();
      setTimeout(() => ws.editor.focus());
    },

    create({ name, sql = "", database, schema, savedId } = {}) {
      const current = this.active();
      const n = this.list.length + 1;
      const model = {
        id: uid(),
        name: name || `Worksheet ${n}`,
        sql,
        database: database !== undefined ? database : current?.model.database ?? null,
        schema: schema !== undefined ? schema : current?.model.schema ?? null,
        savedId,
      };
      const ws = new Worksheet(model);
      this.list.push(ws);
      this.activate(model.id);
      return ws;
    },

    async close(id) {
      const ws = this.list.find((w) => w.model.id === id);
      if (ws.dirty && !(await confirmDialog({ title: "Close worksheet", message: `“${ws.model.name}” has changes that aren't in its saved query. Close it anyway?`, confirmLabel: "Close", danger: true }))) return;
      const index = this.list.indexOf(ws);
      this.list.splice(index, 1);
      ws.endSession();
      if (!this.list.length) this.list.push(new Worksheet({ id: uid(), name: "Worksheet 1", sql: "", database: ws.model.database, schema: ws.model.schema }));
      if (this.activeId === id) this.activate(this.list[Math.min(index, this.list.length - 1)].model.id);
      else this.renderTabs();
      this.persist();
    },

    /** Opens a saved query, in the worksheet already showing it if there is one. */
    openSaved(query) {
      const open = this.list.find((w) => w.model.savedId === query.id);
      if (open) return this.activate(open.model.id);
      const current = this.active();
      if (current && !current.model.sql.trim() && !current.model.savedId) {
        current.model.savedId = query.id;
        current.model.name = query.name;
        current.savedSql = query.sql;
        current.editor.setValue(query.sql);
        if (query.database) current.useContext(query.database, query.schema);
        this.renderTabs();
        return;
      }
      const ws = this.create({ name: query.name, sql: query.sql, database: query.database ?? undefined, schema: query.schema ?? undefined, savedId: query.id });
      ws.savedSql = query.sql;
      this.renderTabs();
    },

    renderTabs() {
      clear(this.tabsEl,
        this.list.map((ws) => {
          const label = h("span.label", null, ws.model.name);
          const tab = h("div.ws-tab", {
            class: ws.model.id === this.activeId ? "active" : "",
            title: ws.model.savedId ? `${ws.model.name} (saved query)` : ws.model.name,
            onClick: () => this.activeId !== ws.model.id && this.activate(ws.model.id),
            onDblclick: () => this.rename(ws, label),
            onAuxclick: (e) => { if (e.button === 1) this.close(ws.model.id); },
          },
          ws.running ? h("span.spinner.spin") : ws.model.savedId ? icon("save") : null,
          label,
          ws.dirty ? h("span.dot", { title: "Changed since saved" }) : null,
          iconButton("x", "Close", () => this.close(ws.model.id)));
          return tab;
        }),
        iconButton("plus", "New worksheet", () => this.create()));
    },

    rename(ws, label) {
      const input = h("input", { type: "text", value: ws.model.name });
      const finish = (keep) => {
        if (keep && input.value.trim()) {
          ws.model.name = input.value.trim();
          if (ws.model.savedId && saved.get(ws.model.savedId)) saved.upsert({ id: ws.model.savedId, name: ws.model.name });
          this.persist();
        }
        this.renderTabs();
      };
      input.addEventListener("keydown", (e) => {
        if (e.key === "Enter") finish(true);
        if (e.key === "Escape") finish(false);
      });
      input.addEventListener("blur", () => finish(true));
      label.replaceWith(input);
      input.select();
    },
  };

  const STARTER_SQL = `-- Welcome to localflake. Run a statement with ${MOD}+Enter.
SELECT CURRENT_DATABASE(), CURRENT_SCHEMA(), CURRENT_USER(), CURRENT_TIMESTAMP();
`;

  const refreshCatalogSoon = debounce(async () => {
    try {
      await catalog.refresh();
    } catch {}
  }, 300);

  // ---------------------------------------------------------------------------
  // Explorer

  const explorer = {
    el: h("div.tree"),
    filterInput: null,
    expanded: new Set(store.get("expanded", [])),
    error: null,

    init() {
      catalog.listeners.add(() => this.render());
      this.reload();
    },

    async reload() {
      this.error = null;
      try {
        await catalog.loadDatabases(true);
        await Promise.all([...this.expanded].filter((k) => k.startsWith("db:")).map((k) => catalog.load(k.slice(3), true).catch(() => {})));
      } catch (err) {
        this.error = err.message;
      }
      this.render();
      worksheets.active()?.renderContext();
    },

    toggle(key, open) {
      if (open ?? !this.expanded.has(key)) this.expanded.add(key);
      else this.expanded.delete(key);
      store.set("expanded", [...this.expanded]);
      this.render();
    },

    render() {
      const filter = (this.filterInput?.value || "").trim().toUpperCase();
      const nodes = [];
      if (this.error) nodes.push(h("div.tree-msg.error", null, this.error));
      else if (!catalog.databases) nodes.push(h("div.tree-msg", null, "Loading…"));
      else if (!catalog.databases.length) nodes.push(h("div.empty", null, "No databases yet.", h("br"), "Create one with the + button, or connect a driver with a database name."));
      for (const db of catalog.databases || []) this.renderDatabase(db, filter, nodes);
      if (filter && catalog.databases && nodes.length === 0) nodes.push(h("div.tree-msg", null, "Nothing matches. The filter searches databases you've expanded."));
      clear(this.el, nodes);
      const ws = worksheets.active();
      ws?.renderContext();
    },

    renderDatabase(db, filter, nodes) {
      const key = "db:" + db.name;
      const data = catalog.byDatabase.get(db.name);
      const open = this.expanded.has(key) || (filter && data);
      const children = [];
      if (open) {
        if (!data) {
          children.push(h("div.tree-msg", { style: { paddingLeft: "38px" } }, "Loading…"));
          catalog.load(db.name).catch((err) => toast(err.message, "error"));
        } else {
          const schemas = [...data.schemas].sort((a, b) => (a.name === "INFORMATION_SCHEMA") - (b.name === "INFORMATION_SCHEMA") || a.name.localeCompare(b.name));
          for (const schema of schemas) this.renderSchema(db.name, schema, filter, children);
        }
      }
      if (filter && !db.name.toUpperCase().includes(filter) && !children.length) return;
      nodes.push(this.node({
        key, depth: 0, label: db.name, iconName: "db", iconClass: "ico-db", open, expandable: true,
        meta: db.comment || "",
        menu: (e) => this.databaseMenu(e, db.name),
        actions: [iconButton("more", "Actions", (e) => this.databaseMenu(e, db.name))],
      }));
      nodes.push(...children);
    },

    renderSchema(database, schema, filter, nodes) {
      const key = `schema:${database}.${schema.name}`;
      const open = this.expanded.has(key) || !!filter;
      const children = [];
      if (open) {
        const groups = [
          ["Tables", schema.objects.filter((o) => !o.kind.includes("VIEW")), "table"],
          ["Views", schema.objects.filter((o) => o.kind.includes("VIEW")), "view"],
          ["Procedures", schema.procedures, "proc"],
          ["Functions", schema.functions, "func"],
          ["Stages", schema.stages, "stage"],
          ["Sequences", schema.sequences, "seq"],
          ["File formats", schema.fileFormats, "format"],
        ];
        for (const [title, items, kind] of groups) {
          const matching = filter ? items.filter((i) => i.name.toUpperCase().includes(filter) || (i.columns || []).some((c) => c.name.toUpperCase().includes(filter))) : items;
          if (!matching.length) continue;
          const groupKey = `group:${database}.${schema.name}:${title}`;
          const groupOpen = !this.expanded.has(groupKey + ":closed") || !!filter;
          children.push(this.node({ key: groupKey, depth: 2, label: `${title}`, meta: String(matching.length), group: true, open: groupOpen, expandable: true,
            onToggle: () => this.toggle(groupKey + ":closed") }));
          if (groupOpen) for (const item of matching) this.renderItem(database, schema.name, item, kind, filter, children);
        }
        if (!children.length && !filter) children.push(h("div.tree-msg", { style: { paddingLeft: "56px" } }, "Empty"));
      }
      if (filter && !schema.name.toUpperCase().includes(filter) && !children.length) return;
      nodes.push(this.node({
        key, depth: 1, label: schema.name, iconName: "schema", iconClass: "ico-schema", open, expandable: true,
        dim: schema.name === "INFORMATION_SCHEMA",
        menu: (e) => this.schemaMenu(e, database, schema.name),
        actions: [iconButton("more", "Actions", (e) => this.schemaMenu(e, database, schema.name))],
      }));
      nodes.push(...children);
    },

    renderItem(database, schema, item, kind, filter, nodes) {
      const name = qualified(database, schema, item.name);
      if (kind === "table" || kind === "view") {
        const key = `obj:${database}.${schema}.${item.name}`;
        const open = this.expanded.has(key) || (!!filter && !item.name.toUpperCase().includes(filter));
        const kindLabel = item.kind.replace("_", " ").toLowerCase();
        nodes.push(this.node({
          key, depth: 3, label: item.name, iconName: kind, iconClass: kind === "view" ? "ico-view" : "ico-table", open, expandable: true,
          title: `${name}\n${kindLabel}${item.rows != null ? ` · ${plural(item.rows, "row")}` : ""}${item.comment ? `\n${item.comment}` : ""}\nDouble-click to insert the name`,
          meta: item.rows != null ? compactNumber(item.rows) : item.kind === "TABLE" ? "" : kindLabel,
          onDouble: () => insertIntoEditor(name),
          menu: (e) => this.objectMenu(e, database, schema, item),
          actions: [
            iconButton("view", "Preview data", () => preview(name)),
            iconButton("more", "Actions", (e) => this.objectMenu(e, database, schema, item)),
          ],
        }));
        if (open) {
          for (const column of item.columns) {
            if (filter && !item.name.toUpperCase().includes(filter) && !column.name.toUpperCase().includes(filter)) continue;
            nodes.push(this.node({
              key: `col:${key}.${column.name}`, depth: 4, label: column.name, iconName: "column", iconClass: "ico-col",
              meta: column.type + (column.nullable ? "" : " NN"),
              title: `${column.name} ${column.type}${column.nullable ? "" : " NOT NULL"}${column.default ? `\nDEFAULT ${column.default}` : ""}${column.comment ? `\n${column.comment}` : ""}`,
              onDouble: () => insertIntoEditor(ident(column.name)),
              menu: (e) => showMenu(e.clientX, e.clientY, [
                { label: "Insert name", icon: "insert", action: () => insertIntoEditor(ident(column.name)) },
                { label: "Copy name", icon: "copy", action: () => copyText(ident(column.name), "Name copied") },
                { label: "Distinct values", icon: "list", action: () => preview(`SELECT ${ident(column.name)}, COUNT(*) AS "COUNT" FROM ${name} GROUP BY 1 ORDER BY 2 DESC LIMIT 100`, true) },
              ], column.name),
            }));
          }
        }
        return;
      }
      const icons = { proc: ["proc", "ico-proc"], func: ["func", "ico-proc"], stage: ["stage", "ico-stage"], seq: ["seq", "ico-seq"], format: ["format", "ico-seq"] };
      const [iconName, iconClass] = icons[kind];
      const meta = kind === "proc" || kind === "func" ? (item.signature || "").replace(/^[^(]*/, "").replace(/ RETURN .*/, "") : item.detail || "";
      nodes.push(this.node({
        key: `${kind}:${database}.${schema}.${item.name}:${item.signature || ""}`, depth: 3, label: item.name, iconName, iconClass,
        meta, title: [name, item.signature, item.description, item.comment].filter(Boolean).join("\n"),
        onDouble: () => insertIntoEditor(kind === "stage" ? "@" + name : name),
        menu: (e) => this.routineMenu(e, database, schema, item, kind),
        actions: [iconButton("more", "Actions", (e) => this.routineMenu(e, database, schema, item, kind))],
      }));
    },

    node({ key, depth, label, iconName, iconClass, open, expandable, meta, title, group, dim, actions = [], menu, onToggle, onDouble }) {
      const toggle = onToggle || (() => this.toggle(key));
      const el = h("div.node", {
        class: [open && "open", group && "group", dim && "dim"].filter(Boolean).join(" "),
        style: { paddingLeft: 6 + depth * 14 + "px" },
        title: title || label,
        onClick: () => (expandable ? toggle() : null),
        onDblclick: (e) => { if (onDouble) { e.preventDefault(); onDouble(); } },
        onContextmenu: (e) => { if (menu) { e.preventDefault(); menu(e); } },
      },
      h("span.twisty", null, expandable ? icon("chevron") : null),
      iconName ? h("span.ico", { class: iconClass }, icon(iconName)) : null,
      h("span.name", null, label),
      meta ? h("span.meta", null, meta) : null,
      actions.length ? h("span.actions", null, actions) : null);
      return el;
    },

    databaseMenu(e, database) {
      showMenu(e.clientX, e.clientY, [
        { label: "Use in worksheet", icon: "target", action: () => worksheets.active().useContext(database, null) },
        { label: "New worksheet here", icon: "plus", action: () => worksheets.create({ database, schema: null }) },
        { label: "Create schema…", icon: "schema", action: () => createObject("schema", database) },
        { label: "Refresh", icon: "refresh", action: () => catalog.load(database, true).catch((err) => toast(err.message, "error")) },
        { label: "Copy name", icon: "copy", action: () => copyText(ident(database), "Name copied") },
        null,
        { label: "Drop database…", icon: "trash", danger: true, action: () => dropObject(`DROP DATABASE ${ident(database)}`, `database ${database}`) },
      ], database);
    },

    schemaMenu(e, database, schema) {
      const name = qualified(database, schema);
      showMenu(e.clientX, e.clientY, [
        { label: "Use in worksheet", icon: "target", action: () => worksheets.active().useContext(database, schema) },
        { label: "Create table (template)", icon: "table", action: () => insertIntoEditor(`CREATE TABLE ${name}.NEW_TABLE (\n  ID NUMBER AUTOINCREMENT,\n  NAME VARCHAR,\n  CREATED_AT TIMESTAMP_NTZ DEFAULT CURRENT_TIMESTAMP()\n);\n`) },
        { label: "Create stage…", icon: "stage", action: () => createObject("stage", database, schema) },
        { label: "Copy name", icon: "copy", action: () => copyText(name, "Name copied") },
        null,
        schema === "INFORMATION_SCHEMA" ? null : { label: "Drop schema…", icon: "trash", danger: true, action: () => dropObject(`DROP SCHEMA ${name}`, `schema ${name}`) },
      ], name);
    },

    objectMenu(e, database, schema, item) {
      const name = qualified(database, schema, item.name);
      const isTable = !item.kind.includes("VIEW") && item.kind !== "DYNAMIC_TABLE";
      const dropKind = { VIEW: "VIEW", MATERIALIZED_VIEW: "MATERIALIZED VIEW", DYNAMIC_TABLE: "DYNAMIC TABLE" }[item.kind] || "TABLE";
      const columns = item.columns.map((c) => ident(c.name));
      showMenu(e.clientX, e.clientY, [
        { label: "Preview data", icon: "view", action: () => preview(name) },
        { label: "Count rows", icon: "hash", action: () => preview(`SELECT COUNT(*) AS ROW_COUNT FROM ${name}`, true) },
        { label: "Describe", icon: "info", action: () => preview(`DESCRIBE ${item.kind.includes("VIEW") ? "VIEW" : "TABLE"} ${name}`, true) },
        null,
        { label: "Insert SELECT statement", icon: "insert", action: () => insertIntoEditor(`SELECT\n  ${columns.join(",\n  ") || "*"}\nFROM ${name}\nLIMIT 100;\n`) },
        isTable ? { label: "Insert INSERT statement", icon: "insert", action: () => insertIntoEditor(`INSERT INTO ${name} (${columns.join(", ")})\nVALUES (${columns.map(() => "NULL").join(", ")});\n`) } : null,
        { label: "Insert name", icon: "insert", action: () => insertIntoEditor(name) },
        { label: "Copy name", icon: "copy", action: () => copyText(name, "Name copied") },
        null,
        isTable ? { label: "Load data from a file…", icon: "upload", action: () => loadDataDialog(database, schema, item) } : null,
        isTable ? { label: "Truncate…", icon: "eraser", danger: true, action: () => dropObject(`TRUNCATE TABLE ${name}`, `every row of ${name}`, "Truncate") } : null,
        { label: `Drop ${dropKind.toLowerCase()}…`, icon: "trash", danger: true, action: () => dropObject(`DROP ${dropKind} ${name}`, `${dropKind.toLowerCase()} ${name}`) },
      ], name);
    },

    routineMenu(e, database, schema, item, kind) {
      const name = qualified(database, schema, item.name);
      const argTypes = /\(([^)]*)\)/.exec(item.signature || "")?.[1] ?? "";
      const items = [];
      const copy = { label: "Copy name", icon: "copy", action: () => copyText(kind === "stage" ? "@" + name : name, "Name copied") };
      if (kind === "proc") {
        items.push(
          { label: "Insert CALL statement", icon: "insert", action: () => insertIntoEditor(`CALL ${name}(${argTypes.split(",").filter((t) => t.trim()).map(() => "NULL").join(", ")});\n`) },
          { label: "Describe", icon: "info", action: () => preview(`DESCRIBE PROCEDURE ${name}(${argTypes})`, true) },
          copy,
          null,
          { label: "Drop procedure…", icon: "trash", danger: true, action: () => dropObject(`DROP PROCEDURE ${name}(${argTypes})`, `procedure ${name}(${argTypes})`) },
        );
      } else if (kind === "func") {
        items.push({ label: "Insert call", icon: "insert", action: () => insertIntoEditor(`${name}()`) }, copy);
      } else if (kind === "stage") {
        items.push(
          { label: "List files", icon: "list", action: () => preview(`LIST @${name}`, true) },
          { label: "Upload file…", icon: "upload", action: () => uploadDialog(`@${name}`) },
          { label: "Query staged files", icon: "insert", action: () => insertIntoEditor(`SELECT METADATA$FILENAME, $1, $2\nFROM @${name}\nLIMIT 100;\n`) },
          copy,
          null,
          { label: "Drop stage…", icon: "trash", danger: true, action: () => dropObject(`DROP STAGE ${name}`, `stage ${name}`) },
        );
      } else if (kind === "seq") {
        items.push(
          { label: "Insert NEXTVAL", icon: "insert", action: () => insertIntoEditor(`${name}.NEXTVAL`) },
          copy,
          null,
          { label: "Drop sequence…", icon: "trash", danger: true, action: () => dropObject(`DROP SEQUENCE ${name}`, `sequence ${name}`) },
        );
      } else if (kind === "format") {
        items.push(
          { label: "Describe", icon: "info", action: () => preview(`DESCRIBE FILE FORMAT ${name}`, true) },
          copy,
          null,
          { label: "Drop file format…", icon: "trash", danger: true, action: () => dropObject(`DROP FILE FORMAT ${name}`, `file format ${name}`) },
        );
      }
      showMenu(e.clientX, e.clientY, items, item.signature || name);
    },
  };

  const compactNumber = (n) => (n < 10000 ? n.toLocaleString() : Intl.NumberFormat("en", { notation: "compact" }).format(n));

  function insertIntoEditor(text) {
    const ws = worksheets.active();
    showView("worksheets");
    ws.editor.insert(text);
    ws.editor.focus();
  }

  /** Runs a query in the active worksheet without touching its SQL. */
  function preview(sqlOrName, isSql = false) {
    const sql = isSql ? sqlOrName : `SELECT * FROM ${sqlOrName} LIMIT 100`;
    showView("worksheets");
    return worksheets.active().runSql(sql, { label: sql });
  }

  async function dropObject(sql, what, verb = "Drop") {
    const ok = await confirmDialog({
      title: verb === "Drop" ? `Drop ${what.split(" ")[0]}` : `${verb} table`,
      message: `This permanently removes ${what}. It runs in the active worksheet's session:`,
      sql,
      confirmLabel: verb,
      danger: true,
    });
    if (!ok) return;
    const data = await worksheets.active().runSql(sql, { label: sql });
    if (data?.error) toast(data.error.message, "error");
    else if (data) toast(`${verb === "Drop" ? "Dropped" : "Truncated"} ${what.replace(/^every row of /, "")}`);
    refreshCatalogSoon();
  }

  async function createObject(kind, database, schema) {
    const titles = { database: "Create database", schema: `Create schema in ${database}`, stage: `Create stage in ${database}.${schema}` };
    const name = await promptDialog({
      title: titles[kind],
      label: "Name",
      placeholder: kind === "database" ? "MY_DB" : kind === "schema" ? "MY_SCHEMA" : "MY_STAGE",
      help: "Unquoted names are upper-cased, as in Snowflake. Use \"quotes\" to keep the case.",
      confirmLabel: "Create",
    });
    if (!name) return;
    const target = kind === "database" ? name : kind === "schema" ? `${ident(database)}.${name}` : `${qualified(database, schema)}.${name}`;
    const sql = `CREATE ${kind.toUpperCase()} ${target}`;
    const ws = worksheets.active();
    // CREATE DATABASE/SCHEMA switch the session to it, as in Snowflake.
    const data = await ws.runSql(sql, { label: sql });
    if (data?.error) toast(data.error.message, "error");
    else if (data) {
      toast(`Created ${kind} ${name}`);
      if (kind !== "stage") explorer.toggle(kind === "database" ? "db:" + (data.context?.database || name) : `schema:${database}.${data.context?.schema}`, true);
    }
    refreshCatalogSoon();
  }

  // ---------------------------------------------------------------------------
  // Uploads: files into stages, and into tables through the user stage

  function fileDropZone(onFile, accept) {
    const input = h("input", { type: "file", accept, hidden: true, onChange: () => input.files[0] && pick(input.files[0]) });
    const label = h("div", null, h("b", null, "Choose a file"), " or drop it here");
    const zone = h("div.drop-zone", { onClick: () => input.click() }, label, input);
    const pick = (file) => {
      clear(label, h("b", null, file.name), ` · ${(file.size / 1024).toFixed(file.size < 10240 ? 1 : 0)} KB`);
      onFile(file);
    };
    zone.addEventListener("dragover", (e) => { e.preventDefault(); zone.classList.add("over"); });
    zone.addEventListener("dragleave", () => zone.classList.remove("over"));
    zone.addEventListener("drop", (e) => {
      e.preventDefault();
      zone.classList.remove("over");
      if (e.dataTransfer.files[0]) pick(e.dataTransfer.files[0]);
    });
    return zone;
  }

  async function uploadFile(stage, file) {
    const ws = worksheets.active();
    const session = await ws.ensureSession();
    const query = `session=${encodeURIComponent(session)}&stage=${encodeURIComponent(stage)}&name=${encodeURIComponent(file.name)}`;
    try {
      return await api(`/stage-files?${query}`, { method: "PUT", raw: file });
    } catch (err) {
      if (err.data?.error?.message) throw new Error(err.data.error.message);
      throw err;
    }
  }

  function uploadDialog(stage) {
    let file = null;
    const path = h("input", { type: "text", placeholder: "optional/sub/directory", spellcheck: "false" });
    dialog({
      title: `Upload to ${stage}`,
      body: [
        fileDropZone((f) => { file = f; }),
        h("label.field", null, h("span", null, "Directory in the stage"), path),
        h("div.help.muted", null, "The file is stored as-is (not compressed), like PUT ... AUTO_COMPRESS = FALSE OVERWRITE = TRUE."),
      ],
      buttons: [
        { label: "Cancel", onClick: (close) => close() },
        {
          label: "Upload", primary: true,
          onClick: async (close) => {
            if (!file) return toast("Choose a file first", "error");
            const target = path.value.trim() ? `${stage}/${path.value.trim().replace(/^\/+|\/+$/g, "")}` : stage;
            try {
              await uploadFile(target, file);
              close();
              toast(`Uploaded ${file.name}`);
              preview(`LIST ${stage}`, true);
            } catch (err) {
              toast(err.message, "error");
            }
          },
        },
      ],
    });
  }

  function loadDataDialog(database, schema, table) {
    const name = qualified(database, schema, table.name);
    let file = null;
    let edited = false;
    const format = h("select", null, ["CSV", "JSON", "PARQUET"].map((f) => h("option", { value: f }, f)));
    const header = h("input", { type: "checkbox", checked: true });
    const delimiter = h("input", { type: "text", value: ",", style: { width: "60px" } });
    const onError = h("select", null, h("option", { value: "ABORT_STATEMENT" }, "Abort on the first error"), h("option", { value: "CONTINUE" }, "Skip bad rows"));
    const sql = h("textarea.plain", { rows: 7, spellcheck: "false", onInput: () => { edited = true; } });
    const csvOptions = h("div.row-fields", null,
      h("label.check", null, header, "First row is a header (match columns by name)"),
      h("label.field", null, h("span", null, "Delimiter"), delimiter));
    const singleVariant = table.columns.length === 1 && ["VARIANT", "OBJECT", "ARRAY"].includes(table.columns[0].type);
    const generate = () => {
      csvOptions.hidden = format.value !== "CSV";
      if (edited) return;
      const fileName = file ? file.name.replace(/'/g, "") : "<file>";
      const options = [];
      if (format.value === "CSV") {
        const delim = delimiter.value === "\\t" || delimiter.value === "\t" ? "\\t" : delimiter.value.replace(/'/g, "''") || ",";
        const formatOptions = [`TYPE = CSV`, `FIELD_OPTIONALLY_ENCLOSED_BY = '"'`];
        if (delim !== ",") formatOptions.push(`FIELD_DELIMITER = '${delim}'`);
        formatOptions.push(header.checked ? "PARSE_HEADER = TRUE" : "SKIP_HEADER = 0");
        options.push(`FILE_FORMAT = (${formatOptions.join(" ")})`);
        if (header.checked) options.push("MATCH_BY_COLUMN_NAME = CASE_INSENSITIVE");
      } else {
        options.push(`FILE_FORMAT = (TYPE = ${format.value})`);
        if (!singleVariant) options.push("MATCH_BY_COLUMN_NAME = CASE_INSENSITIVE");
      }
      options.push(`ON_ERROR = ${onError.value}`, "FORCE = TRUE");
      sql.value = `COPY INTO ${name}\nFROM @~/localflake-uploads\nFILES = ('${fileName}')\n${options.join("\n")};`;
    };
    for (const input of [format, header, delimiter, onError]) input.addEventListener("change", () => { edited = false; generate(); });
    delimiter.addEventListener("input", () => { edited = false; generate(); });
    const zone = fileDropZone((f) => {
      file = f;
      const lower = f.name.toLowerCase().replace(/\.(gz|gzip)$/, "");
      if (/\.(json|ndjson|jsonl)$/.test(lower)) format.value = "JSON";
      else if (lower.endsWith(".parquet")) format.value = "PARQUET";
      else {
        format.value = "CSV";
        if (lower.endsWith(".tsv")) delimiter.value = "\\t";
      }
      edited = false;
      generate();
    }, ".csv,.tsv,.txt,.json,.ndjson,.jsonl,.parquet,.gz");
    generate();
    dialog({
      title: `Load data into ${table.name}`,
      wide: true,
      body: [
        zone,
        h("div.row-fields", null, h("label.field", null, h("span", null, "Format"), format), h("label.field", null, h("span", null, "Errors"), onError)),
        csvOptions,
        h("label.field", null, h("span", null, "COPY statement"), sql,
          h("span.help", null, "The file is uploaded to your user stage (@~/localflake-uploads), then this statement loads it. Edit it to change any option.")),
      ],
      buttons: [
        { label: "Cancel", onClick: (close) => close() },
        {
          label: "Upload and load", primary: true,
          onClick: async (close) => {
            if (!file) return toast("Choose a file first", "error");
            try {
              await uploadFile("@~/localflake-uploads", file);
            } catch (err) {
              return toast(err.message, "error");
            }
            close();
            const data = await worksheets.active().runSql(sql.value, { label: `Loading ${file.name} into ${name}…` });
            if (data && !data.error) toast(`Loaded ${file.name}`);
            refreshCatalogSoon();
          },
        },
      ],
    });
  }

  // ---------------------------------------------------------------------------
  // Sidebar: explorer and saved queries

  function renderSidebar() {
    const tab = store.get("sideTab", "explorer");
    const filter = h("input", { type: "search", placeholder: "Filter objects", onInput: debounce(() => explorer.render(), 120) });
    explorer.filterInput = filter;
    const explorerPane = h("div", { style: { display: "contents" } },
      h("div.side-tools", null, filter,
        iconButton("refresh", "Refresh", () => explorer.reload()),
        iconButton("plus", "Create database", () => createObject("database"))),
      h("div.side-body", null, explorer.el));

    const search = h("input", { type: "search", placeholder: "Search saved queries", onInput: () => renderSaved() });
    const savedList = h("div");
    const importInput = h("input", { type: "file", accept: ".json,application/json", hidden: true, onChange: () => importSaved(importInput) });
    const savedPane = h("div", { style: { display: "contents" } },
      h("div.side-tools", null, search),
      h("div.side-body", null, savedList),
      h("div.side-foot", null,
        h("button.btn.small", { type: "button", title: "Download every saved query as JSON", onClick: () => download("localflake-saved-queries.json", JSON.stringify(saved.items, null, 2), "application/json") }, icon("download"), "Export"),
        h("button.btn.small", { type: "button", title: "Add saved queries from an exported JSON file", onClick: () => importInput.click() }, icon("upload"), "Import"),
        importInput));

    function renderSaved() {
      const needle = search.value.trim().toLowerCase();
      const items = saved.items.filter((q) => !needle || q.name.toLowerCase().includes(needle) || q.sql.toLowerCase().includes(needle));
      if (!saved.items.length) {
        clear(savedList, h("div.empty", null, "No saved queries yet.", h("br"), `Press ${MOD}+S in a worksheet to save its SQL here.`, h("br"), h("span.faint", null, "Saved queries live in this browser's storage.")));
        return;
      }
      clear(savedList, items.map((q) => h("div.saved-item", { onClick: () => worksheets.openSaved(q), title: q.sql },
        h("span.title", null, q.name),
        h("span.when", null, relativeTime(q.updatedAt)),
        h("span.actions", null,
          iconButton("edit", "Rename", async () => {
            const name = await promptDialog({ title: "Rename saved query", label: "Name", value: q.name, confirmLabel: "Rename" });
            if (name) {
              saved.upsert({ id: q.id, name });
              for (const ws of worksheets.list) if (ws.model.savedId === q.id) ws.model.name = name;
              worksheets.renderTabs();
              worksheets.persist();
            }
          }),
          iconButton("trash", "Delete", async () => {
            if (await confirmDialog({ title: "Delete saved query", message: `Delete “${q.name}”? Open worksheets keep their SQL.`, confirmLabel: "Delete", danger: true })) {
              saved.remove(q.id);
              for (const ws of worksheets.list) if (ws.model.savedId === q.id) ws.model.savedId = null;
              worksheets.renderTabs();
              worksheets.persist();
            }
          })),
        h("span.preview", null, q.sql.replace(/\s+/g, " ").slice(0, 200)))));
    }
    saved.listeners.add(renderSaved);
    renderSaved();

    const tabs = h("div.side-tabs");
    const body = h("div", { style: { display: "flex", flexDirection: "column", flex: "1", minHeight: "0" } });
    const select = (name) => {
      store.set("sideTab", name);
      clear(tabs,
        h("button", { type: "button", class: name === "explorer" ? "active" : "", onClick: () => select("explorer") }, "Explorer"),
        h("button", { type: "button", class: name === "saved" ? "active" : "", onClick: () => select("saved") }, "Saved queries ", h("span.count", null, saved.items.length)));
      clear(body, name === "explorer" ? explorerPane : savedPane);
    };
    saved.listeners.add(() => select(store.get("sideTab", "explorer")));
    select(tab);
    return h("aside.sidebar", null, tabs, body);
  }

  async function importSaved(input) {
    const file = input.files[0];
    input.value = "";
    if (!file) return;
    try {
      const items = JSON.parse(await file.text());
      if (!Array.isArray(items)) throw new Error("expected a JSON array of saved queries");
      let added = 0;
      for (const item of items) {
        if (typeof item?.sql !== "string" || typeof item?.name !== "string") continue;
        if (saved.get(item.id)) saved.upsert(item);
        else saved.items.unshift({ ...item, id: item.id || uid(), createdAt: item.createdAt || Date.now(), updatedAt: item.updatedAt || Date.now() });
        added++;
      }
      saved.persist();
      toast(`Imported ${plural(added, "saved query", "saved queries")}`);
    } catch (err) {
      toast(`Cannot import: ${err.message}`, "error");
    }
  }

  function relativeTime(millis) {
    const seconds = (Date.now() - millis) / 1000;
    if (seconds < 45) return "just now";
    if (seconds < 3600) return `${Math.round(seconds / 60)} min ago`;
    if (seconds < 86400) return `${Math.round(seconds / 3600)} h ago`;
    if (seconds < 86400 * 7) return `${Math.round(seconds / 86400)} d ago`;
    return new Date(millis).toLocaleDateString();
  }

  /** Makes an element drag-resizable along an axis. */
  function dragSplitter(handle, axis, onMove, startSize) {
    handle.addEventListener("mousedown", (event) => {
      event.preventDefault();
      const origin = axis === "x" ? event.clientX : event.clientY;
      const start = startSize();
      handle.classList.add("dragging");
      document.body.style.cursor = axis === "x" ? "col-resize" : "row-resize";
      const move = (e) => onMove((axis === "x" ? e.clientX : e.clientY) - origin, start);
      const up = () => {
        handle.classList.remove("dragging");
        document.body.style.cursor = "";
        removeEventListener("mousemove", move);
        removeEventListener("mouseup", up);
      };
      addEventListener("mousemove", move);
      addEventListener("mouseup", up);
    });
  }

  function renderWorksheetsView() {
    const view = $("#view-worksheets");
    view.style.setProperty("--sidebar-w", store.get("sidebarWidth", 280) + "px");
    const sidebar = renderSidebar();
    const splitter = h("div.splitter-v");
    dragSplitter(splitter, "x", (delta, start) => {
      const width = Math.min(600, Math.max(180, start + delta));
      view.style.setProperty("--sidebar-w", width + "px");
      store.set("sidebarWidth", width);
    }, () => sidebar.getBoundingClientRect().width);
    view.append(sidebar, splitter, h("section.workspace", null, worksheets.tabsEl, worksheets.bodyEl));
  }

  // ---------------------------------------------------------------------------
  // Query history

  const history = {
    records: new Map(),
    sequence: 0,
    generation: null,
    selected: null,
    detail: null,
    paused: false,
    limit: 500,
    filters: { text: "", status: "", session: "", hideUi: false },

    async poll() {
      try {
        const data = await api(`/history?since=${this.generation == null ? 0 : this.sequence}`);
        if (this.generation != null && data.generation !== this.generation) {
          this.records.clear();
          this.generation = data.generation;
          this.sequence = 0;
          return this.poll();
        }
        this.generation = data.generation;
        this.sequence = data.sequence;
        for (const record of data.records) this.records.set(record.queryId, record);
        const running = [...this.records.values()].filter((r) => r.status === "running").length;
        const badge = $("#running-count");
        badge.hidden = running === 0;
        badge.textContent = running ? `${running} running` : "";
        badge.className = "count" + (running ? " live" : "");
        if (data.records.length && currentView === "history" && !this.paused) {
          this.renderList();
          if (this.selected && data.records.some((r) => r.queryId === this.selected)) this.loadDetail(this.selected, true);
        }
      } catch {}
    },

    filtered() {
      const { text, status, session, hideUi } = this.filters;
      const needle = text.toLowerCase();
      return [...this.records.values()].reverse().filter((r) =>
        (!status || r.status === status) &&
        (!session || String(r.sessionId) === session) &&
        (!hideUi || !r.client.startsWith("localflake web UI")) &&
        (!needle || r.sql.toLowerCase().includes(needle) || r.queryId.includes(needle) || (r.error?.message || "").toLowerCase().includes(needle) || (r.queryTag || "").toLowerCase().includes(needle)));
    },

    render() {
      const view = $("#view-history");
      const text = h("input", { type: "search", placeholder: "Search SQL, errors, query IDs, tags", value: this.filters.text,
        onInput: debounce(() => { this.filters.text = text.value; this.renderList(); }, 150) });
      const status = h("select", { onChange: () => { this.filters.status = status.value; this.renderList(); } },
        [["", "Any status"], ["running", "Running"], ["succeeded", "Succeeded"], ["failed", "Failed"]].map(([v, l]) => h("option", { value: v, selected: v === this.filters.status }, l)));
      this.sessionSelect = h("select", { onChange: () => { this.filters.session = this.sessionSelect.value; this.renderList(); } });
      const hideUi = h("input", { type: "checkbox", checked: this.filters.hideUi, onChange: () => { this.filters.hideUi = hideUi.checked; this.renderList(); } });
      const pause = h("button.btn", { type: "button", onClick: () => {
        this.paused = !this.paused;
        clear(pause, icon(this.paused ? "play" : "pause"), this.paused ? "Resume live updates" : "Pause");
        if (!this.paused) this.renderList();
      } }, icon(this.paused ? "play" : "pause"), this.paused ? "Resume live updates" : "Pause");
      const clearButton = h("button.btn", { type: "button", onClick: async () => {
        if (!(await confirmDialog({ title: "Clear history", message: "Forget every finished statement in the query history? Stored results stay available to RESULT_SCAN.", confirmLabel: "Clear", danger: true }))) return;
        await api("/history", { method: "DELETE" });
        this.selected = null;
        this.records.clear();
        this.generation = null;
        await this.poll();
        this.render();
      } }, icon("trash"), "Clear");
      this.listEl = h("tbody");
      this.countEl = h("span.muted");
      this.detailEl = h("div.detail");
      this.splitEl = h("div.split", { class: this.selected ? "" : "no-detail" },
        h("div.table-wrap", null,
          h("table.list", null,
            h("thead", null, h("tr", null, ["", "Started", "Duration", "Rows", "Session", "Context", "SQL"].map((t) => h("th", { class: ["Duration", "Rows"].includes(t) ? "num" : "" }, t)))),
            this.listEl)),
        this.detailEl);
      clear(view,
        h("div.filters", null, text, status, this.sessionSelect, h("label.check", null, hideUi, "Hide web UI queries"), this.countEl, h("div.spacer"), pause, clearButton),
        this.splitEl);
      this.renderList();
      if (this.selected) this.loadDetail(this.selected);
      else this.renderDetail();
    },

    renderList() {
      if (!this.listEl) return;
      const sessions = new Map();
      for (const r of this.records.values()) sessions.set(String(r.sessionId), `#${r.sessionId} ${r.client || "unknown client"} (${r.user})`);
      const current = this.filters.session;
      clear(this.sessionSelect, h("option", { value: "" }, "Every session"),
        [...sessions].sort((a, b) => Number(b[0]) - Number(a[0])).map(([id, label]) => h("option", { value: id, selected: id === current }, label)));
      const records = this.filtered();
      this.countEl.textContent = `${plural(records.length, "statement")}${records.length !== this.records.size ? ` of ${this.records.size.toLocaleString()}` : ""}`;
      const shown = records.slice(0, this.limit);
      const rows = shown.map((r) => h("tr.clickable", { class: r.queryId === this.selected ? "selected" : "", onClick: () => this.select(r.queryId) },
        h("td", null, statusBadge(r.status)),
        h("td.mono", { title: new Date(r.startedAt).toLocaleString() }, timeOfDay(r.startedAt)),
        h("td.num", null, r.status === "running" ? h("span.faint", null, formatDuration((Date.now() - r.startedAt) * 1000)) : formatDuration(r.durationMicros)),
        h("td.num", null, r.rows ?? ""),
        h("td", { title: `${r.client} · user ${r.user}` }, h("span", { class: "chip" + (r.client.startsWith("localflake web UI") ? " ui" : "") }, `#${r.sessionId}`), r.client || "unknown client"),
        h("td.mono.muted", null, [r.database, r.schema].filter(Boolean).join(".")),
        h("td.sql", { title: r.sql.slice(0, 2000) }, r.queryTag ? h("span.chip.tag", null, r.queryTag) : null, r.sql.replace(/\s+/g, " ").slice(0, 400))));
      if (records.length > shown.length) {
        rows.push(h("tr", null, h("td", { colspan: 7, style: { textAlign: "center" } },
          h("button.btn.small", { type: "button", onClick: () => { this.limit += 1000; this.renderList(); } }, `Show ${Math.min(1000, records.length - shown.length).toLocaleString()} more`))));
      }
      if (!records.length) {
        rows.push(h("tr", null, h("td", { colspan: 7 }, h("div.empty", null,
          this.records.size ? "No statements match the filters." : "No statements yet. Statements from every client (drivers and worksheets alike) appear here as they run."))));
      }
      clear(this.listEl, rows);
    },

    select(queryId) {
      this.selected = queryId;
      this.splitEl?.classList.remove("no-detail");
      for (const tr of this.listEl.children) tr.classList.remove("selected");
      this.renderList();
      this.loadDetail(queryId);
    },

    open(queryId) {
      this.selected = queryId;
      showView("history");
    },

    async loadDetail(queryId, refresh = false) {
      try {
        const record = await api(`/history/${encodeURIComponent(queryId)}`);
        if (this.selected !== queryId) return;
        if (refresh && this.detail?.queryId === queryId && this.detail.sequence === record.sequence) return;
        this.detail = record;
      } catch (err) {
        this.detail = { missing: true, queryId, message: err.message };
      }
      this.renderDetail();
    },

    renderDetail() {
      if (!this.detailEl) return;
      const record = this.detail;
      if (!this.selected || !record) {
        clear(this.detailEl);
        return;
      }
      if (record.missing) {
        clear(this.detailEl, h("div.note", null, record.message));
        return;
      }
      const tab = this.detailTab || "sql";
      const tabs = [["sql", "SQL"], ["duckdb", `DuckDB SQL (${record.duckdbSql.length})`]];
      if (record.status === "succeeded") tabs.push(["result", "Result"]);
      if (record.error) tabs.push(["error", "Error"]);
      const active = tabs.some(([k]) => k === tab) ? tab : "sql";
      const body = h("div.detail-body");
      const head = h("div.detail-head", null,
        h("div.row", null,
          statusBadge(record.status),
          h("span.qid", null, record.queryId),
          iconButton("copy", "Copy query ID", () => copyText(record.queryId, "Query ID copied")),
          h("div.spacer"),
          record.status === "running" ? h("button.btn.small.danger", { type: "button", onClick: async () => {
            const data = await api(`/queries/${encodeURIComponent(record.queryId)}/cancel`, { method: "POST" });
            toast(data.cancelled ? "Cancelling…" : "It already finished");
          } }, icon("stop"), "Cancel") : null,
          h("button.btn.small", { type: "button", title: "Open this SQL in a new worksheet", onClick: () => {
            worksheets.create({ name: `Query ${record.queryId.slice(0, 8)}`, sql: record.sql + (record.sql.trim().endsWith(";") ? "\n" : ";\n"), database: record.database, schema: record.schema });
            showView("worksheets");
          } }, icon("open"), "Open in worksheet"),
          iconButton("x", "Close", () => { this.selected = null; this.detail = null; this.splitEl.classList.add("no-detail"); this.renderList(); this.renderDetail(); })),
        h("dl.facts", null,
          h("dt", null, "Started"), h("dd", null, new Date(record.startedAt).toLocaleString()),
          h("dt", null, "Duration"), h("dd", null, record.status === "running" ? "running…" : formatDuration(record.durationMicros)),
          h("dt", null, "Session"), h("dd", { title: record.client }, `#${record.sessionId} ${record.client}`),
          h("dt", null, "User"), h("dd", null, record.user),
          h("dt", null, "Context"), h("dd", null, [record.database, record.schema].filter(Boolean).join(".") || "none"),
          h("dt", null, "Rows"), h("dd", null, record.rows ?? "—"),
          h("dt", null, "Kind"), h("dd", null, record.kind || "—"),
          h("dt", null, "Query tag"), h("dd", null, record.queryTag || "—")));
      const tabBar = h("div.detail-tabs", null, tabs.map(([key, label]) =>
        h("button", { type: "button", class: key === active ? "active" : "", onClick: () => { this.detailTab = key; this.renderDetail(); } }, label)));
      if (active === "sql") body.append(codeBlock(record.sql));
      else if (active === "duckdb") {
        if (!record.duckdbSql.length) body.append(h("div.note", null, "This statement didn't run any translated SQL (it was handled by localflake directly, or failed before reaching DuckDB)."));
        else {
          body.append(h("div.note", null, "The DuckDB SQL localflake translated this statement into, in the order it ran. Scripts and procedures run several."));
          record.duckdbSql.forEach((sql, i) => body.append(codeBlock(sql, record.duckdbSql.length > 1 ? `${i + 1} of ${record.duckdbSql.length}` : null)));
        }
      } else if (active === "error") body.append(errorView(record.error));
      else if (active === "result") {
        if (!record.resultAvailable) body.append(h("div.note", null, "This result is no longer stored: localflake keeps the most recent few hundred results (the same ones RESULT_SCAN can read)."));
        else {
          body.append(h("div.note", null, "Loading…"));
          api(`/results/${encodeURIComponent(record.queryId)}`).then((data) => {
            if (data.error) return clear(body, errorView(data.error));
            clear(body, resultPanel(data, { compact: true }));
          }).catch((err) => clear(body, h("div.note", null, err.message)));
        }
      }
      clear(this.detailEl, head, tabBar, body);
    },
  };

  function codeBlock(sql, label) {
    return h("div.codeblock", null,
      label ? h("div.label", null, label) : null,
      h("pre", { html: highlight(sql) }),
      h("div.tools", null, iconButton("copy", "Copy", () => copyText(sql, "SQL copied"))));
  }

  function statusBadge(status) {
    const labels = { running: "Running", succeeded: "Succeeded", failed: "Failed", idle: "Idle" };
    return h("span.status", { class: status }, status === "running" ? h("span.spinner", { style: { width: "9px", height: "9px", borderWidth: "1.5px" } }) : null, labels[status] || status);
  }

  const timeOfDay = (millis) => {
    const d = new Date(millis);
    return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}.${pad(d.getMilliseconds(), 3)}`;
  };

  // ---------------------------------------------------------------------------
  // Sessions

  const sessionsView = {
    sessions: [],

    async poll() {
      try {
        const data = await api("/sessions");
        this.sessions = data.sessions;
        $("#session-count").textContent = this.sessions.length;
        if (currentView === "sessions") this.renderList();
      } catch {}
    },

    render() {
      this.listEl = h("tbody");
      clear($("#view-sessions"),
        h("div.filters", null,
          h("span.muted", { style: { flex: "1" } }, "Every client connected to localflake: drivers, and this page's worksheets. Ending a session makes its client's next request fail with Snowflake's “session no longer exists” error (390111), which is handy for testing reconnects."),
          h("button.btn", { type: "button", onClick: () => this.poll() }, icon("refresh"), "Refresh")),
        h("div.table-wrap", null, h("table.list", null,
          h("thead", null, h("tr", null, ["Session", "Client", "User", "Context", "Logged in", "Last active", "Requests", "Status", ""].map((t) => h("th", { class: t === "Requests" ? "num" : "" }, t)))),
          this.listEl)));
      this.renderList();
    },

    renderList() {
      if (!this.listEl) return;
      if (!this.sessions.length) {
        clear(this.listEl, h("tr", null, h("td", { colspan: 9 }, h("div.empty", null, "No sessions. Connect a driver, or run a query in a worksheet."))));
        return;
      }
      clear(this.listEl, this.sessions.map((s) => h("tr", null,
        h("td.mono", null, `#${s.id}`),
        h("td", null, s.webUi ? h("span.chip.ui", null, "web UI") : null, s.client || h("span.faint", null, "unknown")),
        h("td", null, s.user),
        h("td.mono.muted", null, [s.database, s.schema].filter(Boolean).join(".") || "none"),
        h("td", { title: new Date(s.loggedInAt).toLocaleString() }, relativeTime(s.loggedInAt)),
        h("td", { title: new Date(s.lastActiveAt).toLocaleString() }, relativeTime(s.lastActiveAt)),
        h("td.num", null, s.requests.toLocaleString()),
        h("td", null, statusBadge(s.running ? "running" : "idle")),
        h("td", { style: { textAlign: "right" } },
          h("button.btn.small", { type: "button", onClick: () => {
            history.filters.session = String(s.id);
            showView("history");
          } }, icon("list"), "Queries"), " ",
          h("button.btn.small.danger", { type: "button", onClick: async () => {
            const ok = await confirmDialog({ title: `End session #${s.id}`, message: `End ${s.client || "this client"}'s session${s.running ? " and cancel the statement it's running" : ""}? Its next request fails with error 390111.`, confirmLabel: "End session", danger: true });
            if (!ok) return;
            try {
              await api(`/sessions/${s.id}`, { method: "DELETE" });
              for (const ws of worksheets.list) if (ws.context?.sessionId === s.id) ws.session = null;
              this.poll();
            } catch (err) {
              toast(err.message, "error");
            }
          } }, icon("x"), "End")))));
    },
  };

  // ---------------------------------------------------------------------------
  // Shell: views, theme, shortcuts, polling

  let currentView = "worksheets";

  function showView(name) {
    currentView = name;
    for (const button of document.querySelectorAll("nav.views button")) button.classList.toggle("active", button.dataset.view === name);
    for (const view of ["worksheets", "history", "sessions"]) $(`#view-${view}`).hidden = view !== name;
    if (name === "history") history.render();
    if (name === "sessions") {
      sessionsView.render();
      sessionsView.poll();
    }
    if (name === "worksheets") worksheets.active()?.editor.focus();
    location.hash = name === "worksheets" ? "" : name;
  }

  function setTheme(theme) {
    document.documentElement.dataset.theme = theme;
    store.set("theme", theme);
    clear($("#theme-btn"), icon(theme === "dark" ? "sun" : "moon"));
  }

  function showShortcuts() {
    const keys = (...k) => h("span.keys", null, k.map((key) => h("kbd", null, key)));
    const rows = [
      [keys(MOD, "Enter"), "Run the statement at the cursor, or the selection"],
      [keys(MOD, "Shift", "Enter"), "Run every statement in the worksheet"],
      [keys(MOD, "S"), "Save the worksheet as a saved query"],
      [keys(MOD, "/"), "Comment or uncomment the selected lines"],
      [keys("Ctrl", "Space"), "Complete names: tables, columns (also after “alias.”), keywords"],
      [keys("Tab"), "Indent (Shift+Tab outdents)"],
      [keys("Double-click"), "In the explorer: insert a name. In results: view a value"],
      [keys(MOD, "C"), "In results: copy the selected value"],
      [keys("?"), "Show these shortcuts"],
    ];
    dialog({
      title: "Keyboard shortcuts",
      body: h("div.shortcuts", null, rows.flatMap(([k, text]) => [k, h("span", null, text)])),
      buttons: [{ label: "Close", primary: true, onClick: (close) => close() }],
    });
  }

  async function init() {
    setTheme(document.documentElement.dataset.theme);
    $("#theme-btn").addEventListener("click", () => setTheme(document.documentElement.dataset.theme === "dark" ? "light" : "dark"));
    clear($("#shortcuts-btn"), icon("keyboard"));
    $("#shortcuts-btn").addEventListener("click", showShortcuts);
    for (const button of document.querySelectorAll("nav.views button")) button.addEventListener("click", () => showView(button.dataset.view));
    document.addEventListener("keydown", (e) => {
      if (e.key === "?" && !/INPUT|TEXTAREA|SELECT/.test(document.activeElement?.tagName) && !document.querySelector(".backdrop")) showShortcuts();
    });

    api("/info").then((info) => {
      $("#version").textContent = "v" + info.version;
      const badge = $("#storage-badge");
      if (info.dataDir) {
        badge.textContent = `Persistent · ${info.dataDir}`;
        badge.className = "badge persistent";
        badge.title = "Databases are stored in this directory and survive restarts.";
      } else {
        badge.textContent = "In memory";
        badge.className = "badge memory";
        badge.title = "Everything is lost when localflake stops. Start it with --data-dir to keep databases.";
      }
      document.title = `localflake ${info.version}`;
    }).catch((err) => toast(err.message, "error"));

    worksheets.init();
    renderWorksheetsView();
    explorer.init();

    await history.poll();
    sessionsView.poll();
    setInterval(() => history.poll(), 1500);
    setInterval(() => sessionsView.poll(), 4000);
    // Running statements' durations tick while the history is visible.
    setInterval(() => {
      if (currentView === "history" && !history.paused && [...history.records.values()].some((r) => r.status === "running")) history.renderList();
    }, 1000);

    const initial = location.hash.slice(1);
    if (initial === "history" || initial === "sessions") showView(initial);
    addEventListener("beforeunload", () => worksheets.persist());
  }

  init();
})();
