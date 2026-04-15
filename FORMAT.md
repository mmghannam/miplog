# `orlog-text` format v1

A line-oriented, ASCII-only text representation of a `SolverLog`. Designed to
be human-readable **and** trivially parseable with ~30 lines of code in any
language. The Rust `Display` impl emits this format; `orlog::text::from_text`
reads it back. A round-trip test (`display_roundtrips`) keeps the two honest.

## Grammar

```
<document>  ::= <magic> <newline> <field>* <progress>? <footer>?
<magic>     ::= "orlog-text 1"
<field>     ::= <tag> ":" " " <payload> <newline>
<tag>       ::= [a-z][a-z_]*
<payload>   ::= <tokens> | <freeform>
<tokens>    ::= <token> (" " <token>)*
<token>     ::= <key> "=" <value>
<key>       ::= [a-z][a-z_0-9]*
<value>     ::= "-" | <number> | <bareword> | <quoted>
<number>    ::= [-+]?<digits>("."<digits>)?([eE][-+]?<digits>)?
<bareword>  ::= [A-Za-z0-9._/:+-]+              ; no spaces, no "="
<quoted>    ::= '"' <escaped>* '"'              ; "\\" and '\"' escapes
<freeform>  ::= <bareword> | <quoted>           ; for: problem, status-reason
<comment>   ::= <whitespace>* "#" .* <newline>  ; ignored everywhere
<progress>  ::= <prog_hdr> <prog_col_cmt> <prog_row>*
<prog_hdr>  ::= "progress: rows=" <u> " last_time=" <f|-> <newline>
<prog_col_cmt> ::= "  # cols: time nodes primal dual gap depth lp event"
<prog_row>  ::= "  " <value> (" " <value>)*{9 total}  <newline>
<footer>    ::= "parser: version=" <str> " git=" <str|-> <newline>
```

## Semantics — tags, in emission order

| Tag        | Payload                                                      | Required |
|------------|--------------------------------------------------------------|----------|
| `solver`   | `name=<key> version=<str\|-> git=<hash\|->`                  | yes      |
| `problem`  | freeform (bareword or quoted); whole line if set             | no       |
| `status`   | `<snake_case_status> reason=<str\|->`                        | yes      |
| `timing`   | `wall=<f\|-> cpu=<f\|-> reading=<f\|-> presolve=<f\|-> root_relax=<f\|->` | yes |
| `bounds`   | `primal=<f\|-> dual=<f\|-> gap=<f\|->`                       | yes      |
| `tree`     | `nodes=<u\|-> simplex_iters=<u\|-> sols=<u\|->`              | yes      |
| `presolve` | `rows=<u\|->/<u\|-> cols=<u\|->/<u\|-> nnz=<u\|->/<u\|->`    | yes      |
| `cuts`     | zero or more `<family>=<u>` tokens                           | yes (may be empty) |
| `progress` | `rows=<u> last_time=<f\|->` + indented rows                  | yes (may have 0 rows) |
| `parser`   | `version=<str> git=<hash\|->`                                | yes      |

All values are **required** in emitted output — omissions are represented as
`-` (dash). Parsers should treat a bare `-` as `None`.

## Status enum

`unknown`, `optimal`, `infeasible`, `unbounded`, `infeasible_or_unbounded`,
`time_limit`, `memory_limit`, `other_limit`, `user_interrupt`,
`numerical_error`. Matches serde's `rename_all = "snake_case"`.

## Progress rows

Eight columns in fixed order, separated by single spaces:

```
time nodes primal dual gap depth lp event
```

- `time`, `primal`, `dual`, `gap`: `f64` or `-`. `gap` is a fraction (e.g.
  `0.0423`, not `4.23%`).
- `nodes`, `depth`, `lp`: unsigned integer or `-`.
- `event`: one of `-` (no event), `heuristic`, `branch_solution`, `cutoff`,
  or a quoted raw marker for `NodeEvent::Other` (e.g. `"b"`).

The document always starts with the magic line `orlog-text 1` and ends
(newline-terminated) at the `parser:` line. Parsers must tolerate trailing
whitespace and blank lines at document end.

## Example

```
orlog-text 1
solver: name=scip version=10.0.0 git=-
problem: p_30n20b8
status: optimal reason="optimal solution found"
timing: wall=448.93 cpu=- reading=- presolve=10.16 root_relax=-
bounds: primal=302 dual=302 gap=0
tree: nodes=- simplex_iters=- sols=4
presolve: rows=576/487 cols=18380/4579 nnz=-/-
cuts:
progress: rows=3 last_time=120
  # cols: time nodes primal dual gap depth lp event
  0 0 553 302 0.4539 - - -
  0 38 402 302 0.249 - - heuristic
  120 53747 302 302 0 - - -
parser: version=0.1.0 git=-
```

## Stability guarantee

The format is versioned via the magic line. `orlog-text 1` is stable across
patch and minor releases of the crate. Breaking changes bump the version
number and the old parser variant stays alive for at least one major release.
