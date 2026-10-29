-- Create transactions table
-- Records all balance changes (top-ups, deductions, refunds)

CREATE TABLE IF NOT EXISTS transactions (
    id SERIAL PRIMARY KEY,
    user_id INTEGER NOT NULL,
    amount NUMERIC NOT NULL,
    balance_after NUMERIC NOT NULL,
    description VARCHAR,
    payment_id VARCHAR,
    created_at TIMESTAMP NOT NULL DEFAULT timezone('Asia/Ashgabat', now()),

    CONSTRAINT fk_transactions_user_id FOREIGN KEY (user_id) REFERENCES users(id)
);
