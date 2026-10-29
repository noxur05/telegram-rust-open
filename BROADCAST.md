# Broadcast messages

Ready-to-send announcements for `/admin → 📢 Broadcast`.

**Format:** broadcasts are sent as **plain text** — `admin/broadcast.rs` calls `send_message`
without a `parse_mode`. So:

- no `*bold*` or `_italic_` — asterisks and underscores render literally
- no `\` escaping — a backslash shows up as a backslash
- emoji and line breaks are fine

Copy a block below verbatim.

---

## 1. Automatic payment checking

Announces that the manual "✅ Tölegi barla" button and `/check` are gone and crediting is
now automatic.

```
🔔 Täze mümkinçilik

Indi tölegler awtomatik barlanýar!

Öňler töleg edeniňizden soň "✅ Tölegi barla" düwmesine basmalydyňyz. Indi ol gerek däl.

💳 Pul iberiň — balansyňyz awtomatik artar.
📩 Töleg gelen badyna size habar bereris.

Hiç zat etmeli däl. Sag boluň! 🙏
```

---

## 2. Extend subscription

```
🔄 Täze mümkinçilik: Abunalygy uzaltmak

Indi abunalygyňyzy uzaldyp bilersiňiz — täze abunalyk satyn almak gerek däl.

📋 "Meniň abunalyklarym" → "🔄 Uzaltmak"

✅ VPN salgyňyz üýtgemeýär, programmaňyza täzeden goşmak gerek däl.
♻️ Ulanylmadyk internetiň 60% täze möhlete geçýär.
⚠️ Galan günleriňiz goşulmaýar — täze möhlet şu günden başlaýar.
```

---

## 3. Both in one message

```
🔔 Botda täzelikler

1️⃣ Tölegler awtomatik barlanýar
Pul iberiň — balansyňyz özi artar. "Tölegi barla" düwmesine basmak indi gerek däl, töleg gelende size habar bereris.

2️⃣ Abunalygy uzaltmak
"Meniň abunalyklarym" → "🔄 Uzaltmak". VPN salgyňyz üýtgemeýär, ulanylmadyk internetiň 60% geçýär.

Sag boluň! 🙏
```

---

## 4. Happ migration + outage compensation (2026-08-29)

One message covering all three things: the outage and what was given back, the move to Happ
with a 2-device limit, and the re-import users must do.

**Why re-import is mandatory:** the old links pointed at `example-c.com`, which stopped
answering. The tokens were never revoked, but that host is dead, so an old link cannot work.
Links now issue on `example-b.com` (admin `noxur`) — verified serving HTTP 200.

**Already applied before sending — do not promise these as future work:**
- +2 days and +10GB on all 31 active bot subscriptions (1 unlimited account skipped)
- all 31 converted to Happ, `device_limit=2`, bound devices cleared

Button names below are the exact `tk.json` labels — do not reword them or users won't find them.

```
🔔 Möhüm habar

Geçen gün abunalyk salgylarynda näsazlyk boldy. Ötünç soraýarys.

🎁 Öwezini dolmak üçin ähli işjeň abunalyklara:
   • +2 gün möhlet
   • +10 GB trafik
goşuldy. Munuň üçin hiç zat etmeli däl — eýýäm hasabyňyzda.

📱 Täzelik: Happ programmasy

Hyzmatymyz Happ programmasyna geçdi. Şonuň üçin abunalyk salgyňyz üýtgedi.
Köne salgy indi işlemeýär — täze salgyny almaly.

❗️ Näme etmeli:
1. "📋 Meniň abunalyklarymy" düwmesine basyň
2. Täze salgyny göçüriň
3. Happ programmasyna goşuň

📲 Bir abunalyk bilen 2 enjam ulanyp bilersiňiz.
Öňki enjamlar arassalandy — enjamlaryňyzy täzeden goşmaly bolarsyňyz.

Balansyňyz we abunalygyňyzyň möhleti ýitmedi.

❓ Sorag bolsa "📞 Goldaw" düwmesine basyň.
```

⚠️ Have a native speaker read this before sending — it was drafted by Claude, and it goes to
every user at once with no way to recall it.

---

## Before sending

**Prove the feature first.** Message 1 tells users their balance will fill on its own. As of
writing, that path has never executed against a real payment — the poller is confirmed running
and polling every 30s, but nothing has settled through it yet. If the promise ships before the
proof and it doesn't fire, users stop checking and quietly assume the money is coming.

Same for message 2: no extend has been performed by anyone yet.

Suggested order:

1. Do a small real top-up → confirm the "funds detected" and "balance topped up" messages arrive
2. Do one extend on a cheap plan → confirm the VPN keeps working afterwards
3. Then broadcast

**A broadcast cannot be edited or recalled** once sent, and it goes to every non-bot user in the
`users` table. The confirmation screen shows the recipient count before sending — read it.

**Turkmen wording** comes from the vocabulary already in `locales/tk.json` (`abunalyk`,
`balansyňyz`, `möhlet`, `Meniň abunalyklarym`, `Uzaltmak`) so the terms match what users already
see in the UI. Worth a native-speaker pass before it goes out.

## Delivery notes

- Sent in batches of 25 with a 1.1s pause, retrying up to 3× on Telegram's 429
  (`admin/broadcast.rs`)
- Progress is edited into the status message every 50 users
- The final report lists **every** recipient by id and username, split across 4000-char
  messages — with a large user base that is a lot of follow-up messages to yourself
