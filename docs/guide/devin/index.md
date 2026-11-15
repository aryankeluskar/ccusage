# Devin CLI Data Source (Experimental)

> Devin CLI support is experimental. The sessions database is not a documented format, so expect changes while the [Devin CLI](https://docs.devin.ai/cli) continues to evolve.

ccusage can read Devin CLI usage from its local sessions database. The adapter uses the same focused and unified report shapes as the other local coding CLI data sources.

## Focused Views

::: code-group

```bash [bunx (Recommended)]
bunx ccusage devin --help
```

```bash [npx]
npx ccusage@latest devin --help
```

```bash [pnpm]
pnpm dlx ccusage devin --help
```

:::

## Data Source

The CLI reads assistant responses from the Devin sessions database. `DEVIN_SESSIONS_DIR` names the directory holding `sessions.db` directly, and `DEVIN_DATA_DIR` names the data root that contains `cli/sessions.db`. When neither is set, ccusage checks `~/.local/share/devin/cli/sessions.db` (macOS and Linux) or `%APPDATA%\devin\cli\sessions.db` (Windows).

```bash
DEVIN_DATA_DIR="$HOME/.local/share/devin" ccusage devin daily
```

```text
~/.local/share/devin/
└── cli/
    └── sessions.db
```

## Report Views

| Focused view           | Description                      | See also                                |
| ---------------------- | -------------------------------- | --------------------------------------- |
| `ccusage devin daily`  | Aggregate usage by date          | [Daily Usage](/guide/daily-reports)     |
| `ccusage devin monthly` | Aggregate usage by month         | [Monthly Usage](/guide/monthly-reports) |
| `ccusage devin session` | Group usage by Devin session ID  | [Session Usage](/guide/session-reports) |

These views support `--json`, `--compact`, `--offline`, `--since`, `--until`, and `--timezone`.

## What Gets Calculated

- **Token usage** - Reads input, output, cache read, and cache write token counts from each assistant response.
- **Deduplication** - The Devin CLI re-saves earlier messages when it rebuilds context, so the same response appears under several nodes. ccusage counts each model response once by its request ID.
- **Costs** - Devin records no local costs, because ACU and credit billing live on the Devin account. Costs are estimates from LiteLLM pricing. Devin appends a thinking-effort suffix to model names, so the pricing lookup also tries the base name without the suffix.

## Environment Variables

| Variable             | Description                                                                                     |
| -------------------- | ----------------------------------------------------------------------------------------------- |
| `DEVIN_SESSIONS_DIR` | Directory holding `sessions.db`; the Devin CLI's own relocation variable; comma-separated lists are supported |
| `DEVIN_DATA_DIR`     | Data root that holds `cli/sessions.db`; comma-separated roots are supported                     |
| `LOG_LEVEL`          | Adjust verbosity (0 silent ... 5 trace)                                                         |

## Troubleshooting

::: details No Devin usage data found
Ensure the database exists at `~/.local/share/devin/cli/sessions.db` or `%APPDATA%\devin\cli\sessions.db`. If your database lives elsewhere, set `DEVIN_SESSIONS_DIR` to the directory that contains `sessions.db`, or `DEVIN_DATA_DIR` to the data root that contains `cli/sessions.db`. :::

::: details Costs showing as $0.00
Devin records no local costs, so ccusage estimates costs from LiteLLM pricing. Models that are not in LiteLLM or ccusage's bundled fallback pricing, such as the internal compaction model, report $0.00. :::

::: details Sessions missing after `devin rm`
Reports read the sessions database as it exists now. `devin rm` deletes a session row, and its usage disappears from reports. Restoring is not possible. :::
