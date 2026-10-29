# Marzban Admin Stats Feature

## Overview
Admins (resellers) can view their usage statistics via the bot. The goal is transparency — admins should know they cannot lie about their usage or user counts.

## Marzban API Endpoints Used (Read-Only)
- `GET /api/admins` — list all admins (username, is_sudo, telegram_id, users_usage)
- `GET /api/admin/usage/{username}` — cumulative total bytes (all-time, not per-period)
- `GET /api/users?admin={username}&status=active&limit=1` — get **active** user count only (from `total` field, excludes disabled/expired/limited)

## Database (PostgreSQL, timezone: Asia/Ashgabat in column defaults)

### `marz_admins` table
| Column | Type | Description |
|--------|------|-------------|
| id | SERIAL PK | Auto-increment |
| username | TEXT NOT NULL UNIQUE | Marzban admin username |
| telegram_id | BIGINT NOT NULL UNIQUE | Telegram user ID (set manually by owner) |
| reset_day | INTEGER NOT NULL | Day of month when billing cycle resets (1-31) |
| usage_limit | BIGINT NULL | Monthly usage limit in bytes (NULL = no limit) |
| user_limit | INTEGER NULL | Max active users allowed (NULL = no limit) |

### `marz_snapshots` table
| Column | Type | Description |
|--------|------|-------------|
| id | SERIAL PK | Auto-increment |
| admin_id | INTEGER NOT NULL FK | References marz_admins(id) ON DELETE CASCADE |
| total_usage | BIGINT NOT NULL | Cumulative all-time usage from Marzban API |
| user_count | INTEGER NOT NULL | Number of **active** users under this admin (excludes disabled/expired/limited) |
| created_at | TIMESTAMP NOT NULL | DEFAULT timezone('Asia/Ashgabat', now()) |

Index: `CREATE INDEX idx_marz_snapshots_admin_created ON marz_snapshots(admin_id, created_at DESC)`

### Migration: `006_create_marzban_admin_tables.sql`

## Sync
- Bot syncs admin list from Marzban API (`GET /api/admins`)
- Matches Marzban usernames with `marz_admins` table
- Owner sets `telegram_id`, `reset_day`, `usage_limit`, `user_limit` per admin in DB via SQL

## Snapshots (Every 30 Minutes)
- Background task runs every 30 minutes (tokio::spawn + loop + sleep)
- No graceful shutdown needed — just runs until process exits
- For each admin in `marz_admins`:
  - Fetch `GET /api/admin/usage/{username}` → cumulative bytes
  - Fetch `GET /api/users?admin={username}&status=active&limit=1` → active user count (from `total` field)
  - Insert row into `marz_snapshots`

## Monthly Cycle Calculation

### Reset Day Logic
- Each admin has a `reset_day` (1-31)
- Cycle resets on that day each month
- Edge case: if `reset_day` > last day of month, use last day of month
  - Example: reset_day=31, February → resets on 28th (or 29th leap year)
  - Example: reset_day=31, April → resets on 30th
  - Formula: `min(reset_day, last_day_of_current_month)`
- **All date logic is done in PostgreSQL queries** (no chrono crate needed)

### Usage Calculation
- Marzban returns cumulative all-time usage, not per-period
- Current cycle usage = `latest_snapshot.total_usage - last_snapshot_of_last_reset_day.total_usage`
- "Last snapshot of reset day" = the last snapshot taken on the most recent reset day (e.g., the ~23:30 snapshot)
- This snapshot serves as both END of previous cycle and START of new cycle

### Historical / Previous Month
- Previous month usage = `last_snapshot_of_this_reset_day.total_usage - last_snapshot_of_previous_reset_day.total_usage`
- Always compare last snapshots of consecutive reset days

## First Run / Manual Baselines
- When an admin is added mid-cycle (e.g., today is April 7, reset_day=1), there's no snapshot for April 1
- Owner **manually inserts** the baseline snapshot via SQL:
  ```sql
  INSERT INTO marz_snapshots (admin_id, total_usage, user_count, created_at)
  VALUES (1, 12345678900, 25, '2025-04-01 23:30:00');
  ```
- The owner calculates the correct baseline from Marzban dashboard or API
- **Bot does NOT guess or interpolate** — if no baseline snapshot exists for a reset day, the bot simply shows "no data for this cycle" or similar

## Bot Downtime on Reset Day
- If the bot was down on a reset day and no snapshot exists for that day, the bot does NOT try to guess
- Owner must manually insert the missing baseline snapshot
- This ensures data integrity — better to show "no data" than wrong data

## Anti-Cheating: Peak Tracking
- Admins may try to create extra users mid-cycle and delete them before reset day
- Snapshots every 30 min catch this — the **peak user count** during the cycle is recorded
- Bot queries `MAX(user_count)` from snapshots within current cycle
- Even if admin deletes users before cycle end, the peak is visible to both admin and owner
- This is the primary reason for frequent snapshots — admins should know they are being watched

## Snapshot Cleanup
- Snapshots grow over time (~48/day per admin × N admins)
- **Keep last 2 months** of granular (30-min) snapshots
- For older data: aggregate into one summary row per day (max user_count, last total_usage of that day), then delete the individual snapshots
- Cleanup runs as part of the scheduler loop (e.g., once per day or once per week)
- This keeps the table small while preserving historical cycle comparisons

## `/marz` Command
- Admin sends `/marz` in Telegram
- Bot looks up `telegram_id` in `marz_admins`
- If not found → ignore or "not registered"
- If found → show:

### Stats Display
- **Current cycle usage** vs usage_limit (if set)
- **Previous cycle usage** (for comparison — admin can see if they're trending higher/lower)
- Current **active** user count vs user_limit (if set)
- **Peak user count** this cycle (max from snapshots since last reset)
- Days remaining in cycle
- Warning if over limits

### Things I Might Be Missing (To Discuss)
- Owner-only command to view ALL admins at once?
- Notifications/alerts when admin exceeds limit mid-cycle?
- What language for messages (Turkmen/Russian/English)?
- Should expired/disabled users be visible separately (e.g., "30 active, 5 expired")?

---

## Implementation Plan

### Architecture: Isolated Module
The feature lives in its own module `src/marzban_stats/` — zero impact on existing functionality.

### New Files
```
src/marzban_stats/
├── mod.rs          — module exports
├── db.rs           — all DB queries (marz_admins, marz_snapshots)
├── handler.rs      — /marz command handler (builds stats message)
└── scheduler.rs    — background 30-min snapshot loop + cleanup

migrations/
└── 006_create_marzban_admin_tables.sql
```

**No `cycle.rs` needed** — all date/cycle logic lives in PostgreSQL queries.

### Changes to Existing Files (Minimal)

#### 1. `src/commands.rs` — add `Marz` variant
```rust
pub enum Command {
    Start, Help, Check, Support, Admin,
    Marz,  // NEW
    Unknown(String),
}
// + match "marz" => Some(Command::Marz)
```

#### 2. `src/main.rs` — add route + spawn scheduler
```rust
// In handle_message():
Some(Command::Marz) => marzban_stats::handler::handle_marz(&bot, &db, &msg).await,

// In main():
tokio::spawn(marzban_stats::scheduler::run_snapshot_loop(db.clone()));
```

#### 3. `src/main.rs` — add module declaration
```rust
mod marzban_stats;
```

**That's it.** No changes to handlers/, admin/, auth/, config, i18n, or any other existing module.

### Module Details

#### `marzban_stats/db.rs`
All cycle boundary calculations done in SQL using PostgreSQL date functions.
```
Functions:
- list_marz_admins(conn) -> Vec<MarzAdmin>
- get_marz_admin_by_telegram_id(conn, telegram_id) -> Option<MarzAdmin>
- insert_snapshot(conn, admin_id, total_usage, user_count)
- get_latest_snapshot(conn, admin_id) -> Option<Snapshot>
- get_cycle_stats(conn, admin_id, reset_day) -> CycleStats
  // Single query that returns: current_usage, previous_usage, peak_users,
  // current_user_count, days_remaining
  // Uses PostgreSQL date functions for reset day boundaries
- cleanup_old_snapshots(conn)
  // Aggregate snapshots older than 2 months into daily summaries, then delete originals
```

Key SQL patterns for cycle boundaries:
```sql
-- Effective reset day for a given month (handles short months)
LEAST(reset_day, (DATE_TRUNC('month', d) + INTERVAL '1 month' - INTERVAL '1 day')::DATE - DATE_TRUNC('month', d)::DATE + 1)

-- Last reset date on or before today
-- If today >= effective_reset_day(this_month) → this month's reset day
-- Else → previous month's reset day (with short-month handling)
```

#### `marzban_stats/scheduler.rs`
```
- run_snapshot_loop(conn):
  loop:
    sleep 30 min
    for admin in list_marz_admins():
      usage = GET /api/admin/usage/{admin.username}
      users = GET /api/users?admin={admin.username}&status=active&limit=1 → total
      insert_snapshot(admin.id, usage, users)
    // Once per day: cleanup_old_snapshots()
```
Uses existing `get_marzban_client()` from `auth/mod.rs` — no new HTTP client needed.
No graceful shutdown — scheduler just runs until process exits.

#### `marzban_stats/handler.rs`
```
- handle_marz(bot, conn, msg):
  telegram_id = msg.from.id
  admin = get_marz_admin_by_telegram_id(conn, telegram_id)
  if None → return (ignore)

  stats = get_cycle_stats(conn, admin.id, admin.reset_day)
  // If no baseline snapshot exists → show "no data for this cycle"

  // Format and send message with:
  // - Current usage vs limit
  // - Previous cycle usage
  // - Active users vs limit
  // - Peak users this cycle
  // - Days remaining
  // - Warning if over limits
```

### Dependencies
- **No new crates needed**
- Uses existing: `sea-orm` (DB), `auth/` (Marzban HTTP), `tokio` (scheduler)
- Date calculations: all done in PostgreSQL (no chrono needed)

### Migration SQL
```sql
CREATE TABLE IF NOT EXISTS marz_admins (
    id SERIAL PRIMARY KEY,
    username TEXT NOT NULL UNIQUE,
    telegram_id BIGINT NOT NULL UNIQUE,
    reset_day INTEGER NOT NULL CHECK (reset_day >= 1 AND reset_day <= 31),
    usage_limit BIGINT,
    user_limit INTEGER
);

CREATE TABLE IF NOT EXISTS marz_snapshots (
    id SERIAL PRIMARY KEY,
    admin_id INTEGER NOT NULL REFERENCES marz_admins(id) ON DELETE CASCADE,
    total_usage BIGINT NOT NULL,
    user_count INTEGER NOT NULL,
    created_at TIMESTAMP NOT NULL DEFAULT timezone('Asia/Ashgabat', now())
);

CREATE INDEX IF NOT EXISTS idx_marz_snapshots_admin_created
    ON marz_snapshots(admin_id, created_at DESC);
```

### Admins are populated manually via SQL:
```sql
INSERT INTO marz_admins (username, telegram_id, reset_day, usage_limit, user_limit)
VALUES ('myrat', 123456789, 5, 107374182400, 30);
-- usage_limit = 100 GB in bytes, user_limit = 30
```

### Manual baseline snapshot (for admins added mid-cycle):
```sql
-- Owner calculates correct values from Marzban dashboard/API
INSERT INTO marz_snapshots (admin_id, total_usage, user_count, created_at)
VALUES (1, 12345678900, 25, '2025-04-01 23:30:00');
```
