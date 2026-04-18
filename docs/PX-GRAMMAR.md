# Praxis Intent Language (.px) — Grammar Specification v0.1

## Primitives

```
document     = (statement NEWLINE*)*
statement    = fact_decl | event_decl | rule_decl | constraint_decl 
             | contract_decl | function_decl | import_decl | trigger_decl

# === Imports ===
import_decl  = "import" rust_path ("as" IDENT)?
rust_path    = IDENT ("::" IDENT)*

# === Facts ===
fact_decl    = "fact" IDENT ":" NEWLINE INDENT field_list DEDENT
field_list   = (field NEWLINE)*
field        = IDENT ":" type_expr
type_expr    = "bool" | "int" | "float" | "string" | "duration"
             | "enum(" IDENT ("," IDENT)* ")"
             | "list[" type_expr "]"
             | "optional[" type_expr "]"

# === Events ===
event_decl   = "event" IDENT ":" NEWLINE INDENT field_list DEDENT

# === Rules ===
rule_decl    = "rule" IDENT ":" NEWLINE INDENT rule_body DEDENT
rule_body    = ("priority:" INT NEWLINE)?
               "when:" NEWLINE INDENT condition_list DEDENT
               (let_clause)*
               "then:" NEWLINE INDENT action_list DEDENT
               ("capture:" NEWLINE INDENT capture_list DEDENT)?
condition_list = ("- " expr NEWLINE)*
let_clause   = "let" IDENT "=" expr NEWLINE
action_list  = ("- " action_stmt NEWLINE)*
action_stmt  = "action:" IDENT (param_pair)*
             | "if" expr ":" action_stmt
param_pair   = IDENT ":" value

# === Constraints ===
constraint_decl = "constraint" IDENT ":" NEWLINE INDENT constraint_body DEDENT
constraint_body = ("scope:" IDENT NEWLINE)?
                  "when:" expr NEWLINE
                  "require:" expr NEWLINE
                  "severity:" ("error" | "warning" | "info") NEWLINE
                  ("message:" STRING NEWLINE)?

# === Contracts ===
contract_decl = "contract" IDENT ":" NEWLINE INDENT contract_body DEDENT
contract_body = ("given:" STRING NEWLINE)?
                ("when:" STRING NEWLINE)?
                ("then:" STRING NEWLINE)?
                ("threshold:" FLOAT NEWLINE)?
                "examples:" NEWLINE INDENT example_list DEDENT
example_list  = ("- " example NEWLINE)*
example       = "input:" value NEWLINE
                "expect:" value (NEWLINE "threshold:" FLOAT)?

# === Functions ===
function_decl = "function" IDENT "(" param_list ")" "->" type_expr ":" NEWLINE
                INDENT function_body DEDENT
function_body = ("mode:" ("deterministic" | "probabilistic" | "hybrid") NEWLINE)?
                DOCSTRING
param_list    = (param ("," param)*)?
param         = IDENT ":" type_expr

# === Triggers ===
trigger_decl  = "trigger" IDENT ":" NEWLINE INDENT trigger_body DEDENT
trigger_body  = "on:" trigger_event NEWLINE
                ("schedule:" cron_or_interval NEWLINE)?
                "run:" IDENT
trigger_event = "after_store" | "before_search" | "on_event" "(" STRING ")"
              | "timer"
cron_or_interval = STRING  # "*/15 * * * *" or "every 30s"

# === Expressions ===
expr         = comparison (("and" | "or") comparison)*
comparison   = term (("==" | "!=" | ">" | "<" | ">=" | "<=") term)?
term         = IDENT ("." IDENT)* | call_expr | value | "(" expr ")"
             | "NOT" term
call_expr    = IDENT "(" (expr ("," expr)*)? ")"
value        = STRING | INT | FLOAT | BOOL | enum_val | list_val | map_val
enum_val     = IDENT
list_val     = "[" (value ("," value)*)? "]"
map_val      = "{" (IDENT ":" value ("," IDENT ":" value)*)? "}"

# === Tokens ===
IDENT        = [a-zA-Z_][a-zA-Z0-9_]*
STRING       = '"' [^"]* '"' | "'" [^']* "'"
INT          = [0-9]+
FLOAT        = [0-9]+ "." [0-9]+
BOOL         = "true" | "false"
DOCSTRING    = '"""' .* '"""'
NEWLINE      = '\n'
INDENT       = increase in indentation
DEDENT       = decrease in indentation
```

## Compilation Target

Each primitive compiles to a PluresDB record:

| .px Primitive | PluresDB Node Type | Key Fields |
|---|---|---|
| fact | schema/fact/{name} | fields, types |
| event | schema/event/{name} | fields, types |
| rule | rule/{name} | conditions, actions, priority |
| constraint | constraint/{name} | when, require, severity |
| contract | contract/{name} | examples, threshold |
| function (deterministic) | function/{name} | rust_source, signature |
| function (probabilistic) | function/{name} | prompt, model, signature |
| trigger | trigger/{name} | event, schedule, target |
