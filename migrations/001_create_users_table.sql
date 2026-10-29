-- Create users table
-- Stores registered Telegram users with their balance

CREATE TABLE IF NOT EXISTS users (
    id SERIAL PRIMARY KEY,
    username VARCHAR,
    telegram_id BIGINT NOT NULL UNIQUE,
    is_bot BOOLEAN NOT NULL DEFAULT false,
    balance NUMERIC NOT NULL DEFAULT 0,
    created_at TIMESTAMP NOT NULL DEFAULT timezone('Asia/Ashgabat', now())
);
