# VPN Subscription Bot — Full Documentation

## Table of Contents

1. [Overview](#1-overview)
2. [User Onboarding](#2-user-onboarding)
3. [Main Menu](#3-main-menu)
4. [Balance System](#4-balance-system)
5. [Payment Processing](#5-payment-processing)
6. [Subscription Purchase](#6-subscription-purchase)
7. [My Subscriptions](#7-my-subscriptions)
8. [Help System](#8-help-system)
9. [Support System](#9-support-system)
10. [Language System](#10-language-system)
11. [Rate Limiting](#11-rate-limiting)
12. [Admin Panel](#12-admin-panel)
13. [VPN Account Provisioning](#13-vpn-account-provisioning)
14. [Security Model](#14-security-model)
15. [Data Model](#15-data-model)
16. [Configuration Reference](#16-configuration-reference)

---

## 1. Overview

A Telegram-based storefront for purchasing and managing VPN subscriptions. Users interact entirely through the Telegram chat interface — there is no web dashboard. The bot handles user registration, cryptocurrency payments, balance management, multi-plan subscription purchases, and VPN account provisioning on a remote panel.

Administrators manage the system through a built-in admin panel accessible via the `/admin` command, which provides user management, financial oversight, subscription plan configuration, and broadcast messaging.

The bot supports three languages (Turkmen, Russian, English) and all user-facing text is fully localized.

---

## 2. User Onboarding

### First Interaction

When a user sends `/start` or opens the bot for the first time:

1. The user's Telegram profile data is saved: Telegram ID, username, and bot status.
2. The account is created with a zero balance.
3. The default language is set to Turkmen.
4. The main menu is displayed with a persistent reply keyboard.

If the user already exists, the system does nothing (no duplicate accounts). The main menu is shown regardless.

### Stored User Data

| Field | Description |
|---|---|
| Telegram ID | Unique identifier from Telegram |
| Username | Telegram @username (optional) |
| Bot flag | Whether the account belongs to a bot |
| Balance | Current USD balance (starts at 0) |
| Language | Preferred language code (tk/ru/en) |
| Created at | Registration timestamp |

---

## 3. Main Menu

The main menu is a persistent reply keyboard always visible at the bottom of the chat. It contains six buttons arranged in three rows:

| Row | Buttons |
|---|---|
| 1 | **Balance** — **Buy subscription** |
| 2 | **My Subscriptions** |
| 3 | **Help** — **Support** — **Language** |

Button labels are displayed in the user's selected language. Pressing any button cancels any active input mode (support message, custom top-up amount).

---

## 4. Balance System

### Checking Balance

Pressing the **Balance** button displays the current USD balance with an inline **Top Up** button.

### Top-Up Flow

1. User presses **Top Up**.
2. An amount selection screen appears with four options:
   - **$5** / **$10** / **$15** — preset amounts
   - **Custom** — opens a text input mode
3. For custom amounts, the user types a number between 1 and 100 USD.
4. A cryptocurrency payment is created via the payment gateway.

### Payment Creation

When an amount is selected:

- A cryptocurrency payment order is generated.
- The bot displays:
  - **Order amount** in USD
  - **Pay amount** in the configured cryptocurrency
  - **Wallet address** to send funds to
  - **Payment ID** for reference
- A **Check payment** button is shown.
- The pending payment is stored in the database (one per user, replaced on new payment).
- Payments expire after **24 hours**.

### Partial Payments

The system accepts partial payments. If the user sends less than the requested cryptocurrency amount, the balance is topped up proportionally. For example:

- Payment order: $10
- User sends crypto worth $3
- Balance increases by $3

Users can accumulate balance across multiple payments until they have enough to purchase a subscription.

---

## 5. Payment Processing

### Checking Payment Status

Users can check their payment in two ways:

- **Automatic**: Press the **Check payment** button (uses the most recent pending payment).
- **Manual**: Send `/check <payment_id>` with a specific payment ID.

### Payment Statuses

| Status | Behavior |
|---|---|
| **Finished** | Full payment received. Balance topped up by the full amount. |
| **Partially paid** | Partial crypto received. Balance topped up proportionally. |
| **Waiting** | No funds received yet. User prompted to send and check again. |
| **Confirming** | Funds received, awaiting blockchain confirmations. Check again shortly. |
| **Sending** | Payment being processed. Check again shortly. |
| **Expired** | Payment window (24h) passed. User prompted to create a new payment. |
| **Refunded** | Funds returned by the payment processor. User prompted to retry. |
| **Failed** | Payment failed. User prompted to retry. |

### Idempotency Protection

Each payment can only be credited to a user's balance **once**. The system prevents duplicate processing through:

1. A uniqueness check on the payment ID before crediting.
2. An atomic database operation that inserts the transaction record and updates the balance in a single step — if the payment ID already exists, the operation is rejected.

### Ownership Validation

When a user checks a payment manually by ID, the system verifies that the payment's embedded order ID contains the user's Telegram ID. Payments belonging to other users are rejected with a "not yours" message.

---

## 6. Subscription Purchase

### Multi-Plan Selection

The subscription purchase flow allows users to choose from all available plans:

1. User presses **Buy subscription** (keyboard button or inline button).
2. The bot fetches all subscription plans from the database.
3. Each plan is displayed as an inline button with the format:
   ```
   Type Name — $Price | Days | Data GB
   ```
4. If no plans exist, a "no subscription available" message is shown.

### Purchase Execution

When the user selects a plan:

1. **Balance check & deduction**: The user's balance is atomically reduced by the plan's price. If the balance is insufficient, the purchase is rejected with a message showing the required price.

2. **VPN account creation**: A new VPN account is provisioned on the remote panel with:
   - A unique username derived from the user's Telegram ID and timestamp
   - The plan's duration (converted to an expiration timestamp)
   - The plan's data limit (converted to bytes)
   - All available protocols and inbounds auto-detected from the panel

3. **Success**: The user receives:
   - Their subscription URL (for importing into a VPN client app)
   - Duration and data limit confirmation
   - Remaining balance after purchase

### Failure & Refund

If the VPN account creation fails **after** the balance was already deducted, the system automatically refunds the full amount. Failure scenarios include:

- Unable to fetch available protocols from the VPN panel
- Connection error to the VPN panel
- VPN panel rejects the account creation request

Each refund failure is logged with the user's ID and the amount for manual investigation.

---

## 7. My Subscriptions

Pressing **My Subscriptions** displays all VPN accounts belonging to the user, fetched from the remote VPN panel by searching for accounts matching the user's Telegram ID pattern.

### Information Per Subscription

Each subscription shows:

| Field | Description |
|---|---|
| **Status** | Active, Expired, Data limit reached, Disabled, On hold, or Unknown — with a colored emoji indicator |
| **Data usage** | Used traffic vs. total limit in GB (or unlimited) |
| **Time remaining** | Days and hours until expiration (or "Expired") |
| **Subscription URL** | Tap-to-copy URL for importing into a VPN client |

### Display Format

```
📋 My Subscriptions

━━━━━━━━━━━━━━━━━━━━━━
🟢 Subscription #1

📌 Status: Active
📊 Used: 15.50 / 100.00 GB
⏰ Time left: 25 days, 12 hours

🔗 URL: https://...

━━━━━━━━━━━━━━━━━━━━━━
📊 Total: 2 subscriptions
```

If the user has no subscriptions, a prompt to purchase one is displayed.

---

## 8. Help System

The `/help` command (or **Help** button) opens an interactive help menu with five sections, each accessible via inline buttons:

### Sections

| Section | Content |
|---|---|
| **Pricing** | Lists all available subscription plans with price, duration, and data limit for each |
| **Top Up** | Explains the cryptocurrency top-up process, partial payments, and minimum price needed |
| **How to Buy** | Step-by-step guide: top up → check payment → buy subscription → import URL |
| **Commands** | Lists all available bot commands (/start, /help, /check, /support) |
| **Support** | Instructions for contacting the support team |

Each section includes a **Back to Help** button to return to the menu.

The Pricing section dynamically shows all plans from the database. If no plans exist, a "no plans available" message is shown. The Top Up and How to Buy sections reference the lowest available price.

---

## 9. Support System

### User-to-Admin Messaging

1. User presses **Support** button or sends `/support`.
2. Bot prompts: "Type your message, we will reply."
3. User types their message (text only — media is rejected).
4. The message is forwarded to the configured support group chat with:
   - User's full name and @username
   - User's Telegram ID
   - The message text

### Admin Reply

1. In the support group, an admin **replies** to the bot's forwarded message.
2. The bot extracts the original user's Telegram ID from the forwarded message.
3. The reply is sent to the user as a "Support reply" message in their preferred language.
4. The admin receives a confirmation that the reply was delivered.

### Availability

If no support group is configured, users see a "Support unavailable" message.

### State Management

- The support input mode is automatically cancelled when the user presses any keyboard button or enters a command.
- A cancellation message is shown when the mode is exited.

---

## 10. Language System

### Available Languages

| Code | Language | Role |
|---|---|---|
| **tk** | Turkmen | Default language, ultimate fallback |
| **ru** | Russian | Full feature parity |
| **en** | English | Full feature parity |

### Language Selection

1. User presses the **Language** button.
2. Three flag-labeled inline buttons are shown.
3. Selecting a language:
   - Saves the preference to the database
   - Immediately sends the main menu in the new language
   - All subsequent messages use the selected language

### Fallback Behavior

If a translation key is missing for the user's language, the system falls back to Turkmen. All 92+ translation keys are present in all three languages.

### Button Matching

Keyboard button presses are matched across all languages simultaneously. A user who switches from Russian to English will still have their old Russian-labeled keyboard buttons recognized until the menu refreshes.

---

## 11. Rate Limiting

### Mechanism

A per-user token bucket rate limiter protects against spam and abuse:

| Parameter | Value |
|---|---|
| **Max requests** | 15 per window |
| **Window duration** | 60 seconds |
| **Replenishment** | Proportional to elapsed time |

### Behavior

- When a user exceeds the limit, all messages and button presses are rejected.
- The user sees: "Too many requests. Please wait X seconds."
- For callback buttons, the rejection appears as a Telegram popup alert.
- The limiter self-cleans when its internal storage exceeds 1,000 entries, removing stale records older than 2 minutes.

---

## 12. Admin Panel

The admin panel is accessible only to the configured administrator via the `/admin` command. All admin features are protected by Telegram ID verification.

### 12.1 Dashboard Stats

Displays a real-time overview:

- **Users**: Total non-bot user count
- **Transactions**: Total transaction count
- **Total revenue**: Sum of all positive transaction amounts
- **Pending payments**: Count of unexpired pending payments

### 12.2 User Listing

A paginated view of all registered users (6 per page), ordered by newest first.

Each entry shows: `@username | $balance USD`

Clicking a user opens their detail view. Navigation buttons allow paging through the full list.

### 12.3 User Detail View

Displays comprehensive user information:

- Internal ID, username, Telegram ID, balance, registration date
- **Paginated transaction history** (6 per page) showing amount, date, and description
- Clicking a transaction opens its detail view

### 12.4 Transaction Listing

A paginated view of all transactions across all users (6 per page), ordered by newest first.

Each entry shows: `$amount | @username | date`

Clicking a transaction opens its detail view.

### 12.5 Transaction Detail

Displays full transaction information:

- Transaction ID, amount, balance after, user (with @username and Telegram ID), description, date

### 12.6 User Lookup

Allows the admin to look up any user by their Telegram ID:

1. Admin enters a Telegram ID.
2. If found: user info + last 10 transactions are displayed.
3. If not found: a warning message is shown.

### 12.7 Balance Management

Allows the admin to adjust any user's balance:

1. Admin enters the target user's Telegram ID.
2. System shows the user's current balance.
3. Admin enters an adjustment amount:
   - Positive number to **add** funds
   - Negative number to **subtract** funds
4. A confirmation screen displays:
   - Current balance, action (add/subtract), and projected new balance
5. Admin confirms or cancels.
6. The adjustment is recorded as a transaction with the description "Admin adjustment by {admin_id}".

**Safety**: Negative adjustments cannot reduce the balance below zero.

### 12.8 Broadcast Messaging

Allows the admin to send a message to all registered users:

1. Admin clicks **Broadcast** and types a message.
2. A confirmation screen shows the total user count and message preview.
3. On confirmation, messages are sent in batches:
   - **Batch size**: 25 messages
   - **Delay**: 1.1 seconds between batches (respects Telegram rate limits)
   - **Retry**: Up to 3 retries on rate-limit errors
   - **Progress**: Reported every 50 users
4. A delivery report is generated showing:
   - Successfully delivered users (with @username)
   - Failed deliveries (with error reason)
   - The report is split into multiple messages if it exceeds the Telegram message size limit.

### 12.9 Subscription Type Management

Subscription types are categories that group subscription plans (e.g., "Basic", "Premium", "VIP").

**Create a type**: Enter a unique name. Duplicate names are rejected.

**View a type**: Shows all subscription plans under that type, with buttons to add plans or delete the type.

**Delete a type**: Requires confirmation. Deleting a type also deletes **all subscription plans** under it (cascade).

### 12.10 Subscription Plan Management

Subscription plans define the actual purchasable offerings.

**Create a plan**:

1. Select a subscription type.
2. Enter price in USD (e.g., 2.50).
3. Enter duration in days (e.g., 30).
4. Enter data limit in GB (e.g., 100).
5. Confirm the details.

**Edit a plan**: Same flow as creation — enter new price, days, and data limit. Existing values are replaced.

**Delete a plan**: Requires confirmation. The plan is permanently removed.

**Availability**: All plans in the database are available to users. To make a plan unavailable, the admin deletes it. There is no active/inactive toggle — if it exists, users can buy it.

---

## 13. VPN Account Provisioning

When a user purchases a subscription, the system provisions a VPN account on the remote panel:

### Account Parameters

| Parameter | Value |
|---|---|
| **Username** | `tg_{telegram_id}_{unix_timestamp}` — unique per purchase |
| **Status** | Active |
| **Expiration** | Current time + plan duration in days |
| **Data limit** | Plan's GB limit converted to bytes |
| **Reset strategy** | No reset (one-time data allocation) |
| **Note** | `TG:{telegram_id} Balance` — for admin reference |

### Protocol Auto-Detection

The system queries the VPN panel to discover all available protocols and their configurations:

- **Protocol types**: VLESS, Trojan, Shadowsocks, and any others supported by the panel.
- **TLS detection**: If any inbound uses Reality TLS, the corresponding protocol is configured with `xtls-rprx-vision` flow.
- **Inbound tags**: All available inbound tags are collected per protocol and assigned to the new account.

This means the bot automatically adapts to whatever protocols the VPN panel administrator has configured — no manual protocol configuration is needed in the bot.

### Subscription URL

After successful creation, the VPN panel returns a subscription URL. This URL is displayed to the user for importing into their VPN client application (e.g., V2rayNG, Streisand, Hiddify).

---

## 14. Security Model

### Access Control

- **Bot filtering**: Messages and callbacks from bot accounts are rejected at the middleware level before any processing.
- **Admin authentication**: All admin features verify the caller's Telegram ID against the configured admin ID. Non-admin users clicking admin buttons see no response.
- **Support validation**: Only messages in the configured support group chat trigger reply forwarding.

### Financial Safety

- **Atomic balance deduction**: When purchasing a subscription, the balance is reduced in a single conditional database operation that checks `balance >= price`. This prevents double-spending even under concurrent requests.
- **Atomic top-up**: Balance top-ups use a single database operation that simultaneously inserts the transaction record and updates the balance, preventing the same payment from being credited twice.
- **Automatic refunds**: If VPN account creation fails after balance deduction, the full amount is automatically refunded with error logging.
- **Refund failure logging**: If a refund itself fails, a critical-level log is emitted with the user ID and amount for manual investigation.

### Payment Security

- **One-time use**: Each payment ID can only credit a balance once, enforced at the database level.
- **Ownership verification**: Manual payment checks validate that the payment belongs to the requesting user by checking the embedded Telegram ID in the order identifier.
- **Expiry**: Pending payments expire after 24 hours and are cleaned up from the database.

### Input Handling

- All user-facing text is escaped for Telegram's MarkdownV2 format to prevent formatting injection.
- Numeric inputs (amounts, IDs) are parsed with strict validation — invalid inputs prompt the user to retry.

---

## 15. Data Model

### Users

Stores all registered Telegram users.

| Column | Type | Constraints |
|---|---|---|
| id | Auto-increment integer | Primary key |
| username | Text | Optional |
| telegram_id | 64-bit integer | Unique, not null |
| is_bot | Boolean | — |
| balance | Decimal | Default 0 |
| language | Text | Default 'tk' |
| created_at | Timestamp | Asia/Ashgabat timezone |

### Transactions

Records all balance changes (top-ups, purchases, refunds, admin adjustments).

| Column | Type | Constraints |
|---|---|---|
| id | Auto-increment integer | Primary key |
| user_id | Integer | Foreign key → Users |
| amount | Decimal | Positive = credit, negative = debit |
| balance_after | Decimal | Balance after this transaction |
| description | Text | Human-readable reason |
| payment_id | Text | Unique (nullable, set for top-ups) |
| created_at | Timestamp | Asia/Ashgabat timezone |

### Pending Payments

Tracks active cryptocurrency payment orders awaiting confirmation.

| Column | Type | Constraints |
|---|---|---|
| id | Auto-increment integer | Primary key |
| payment_id | Text | Unique |
| telegram_user_id | 64-bit integer | Unique (one pending per user) |
| created_at | Timestamp | — |
| expires_at | Timestamp | 24 hours after creation |

### Subscription Types

Categories for organizing subscription plans.

| Column | Type | Constraints |
|---|---|---|
| id | Auto-increment integer | Primary key |
| name | Text | Unique, not null |

### Subscriptions

Available subscription plans that users can purchase.

| Column | Type | Constraints |
|---|---|---|
| id | Auto-increment integer | Primary key |
| subscription_type_id | Integer | Foreign key → Subscription Types (cascade delete) |
| price | Float | USD price |
| days | Integer | Duration in days |
| limit_gb | Float | Data limit in gigabytes |

---

## 16. Configuration Reference

The following configuration values must be set for the bot to operate:

### Required

| Variable | Description | Example |
|---|---|---|
| `TELEGRAM_BOT_API` | Telegram Bot API token | `123456:ABC-DEF...` |
| `DATABASE_URL` | Database connection string | `postgresql://user:pass@host/db` |
| `MARZBAN_HOST` | VPN panel base URL | `https://panel.example.com` |
| `MARZBAN_USERNAME` | VPN panel admin username | `admin` |
| `MARZBAN_PASSWORD` | VPN panel admin password | `secret` |
| `NOWPAYMENTS_API_KEY` | Payment gateway API key | `ABC123...` |

### Optional

| Variable | Description | Default |
|---|---|---|
| `ADMIN_TELEGRAM_ID` | Telegram ID of the administrator | None (admin panel disabled) |
| `SUPPORT_CHAT_ID` | Telegram group ID for support messages | None (support disabled) |
| `SUBSCRIPTION_URL` | Base URL for VPN subscription links | None |
| `PRICE_CURRENCY` | Fiat currency for payment orders | `USD` |
| `PAY_CURRENCY` | Cryptocurrency accepted for payment | `USDTBSC` |
| `DESCRIPTION` | Payment order description shown to user | `VPN subscription top-up` |
| `MARZBAN_INBOUND_TAG` | Legacy inbound tag override | Auto-detected |
| `MARZBAN_PROXY_TYPE` | Legacy proxy type override | Auto-detected |
| `SUBSCRIPTION_PRICE` | Legacy single-plan price | Replaced by database plans |
| `SUBSCRIPTION_DAYS` | Legacy single-plan duration | Replaced by database plans |
| `SUBSCRIPTION_DATA_LIMIT_GB` | Legacy single-plan data limit | Replaced by database plans |

> **Note**: The legacy single-plan configuration variables (`SUBSCRIPTION_PRICE`, `SUBSCRIPTION_DAYS`, `SUBSCRIPTION_DATA_LIMIT_GB`, `MARZBAN_INBOUND_TAG`, `MARZBAN_PROXY_TYPE`) are no longer used by the subscription purchase flow. Subscription plans are now managed entirely through the admin panel's database-driven system.

---

## Bot Commands

| Command | Scope | Description |
|---|---|---|
| `/start` | All users | Register and show main menu |
| `/help` | All users | Open the interactive help menu |
| `/check` | All users | Check latest pending payment status |
| `/check <id>` | All users | Check a specific payment by ID |
| `/support` | All users | Enter support messaging mode |
| `/admin` | Admin only | Open the admin panel |
