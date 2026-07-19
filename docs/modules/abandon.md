Ownership-checked: an agent may only release its own claim. Returns the
task to `pending` rather than guessing whether its deps still hold;
`refresh_ready` promotes it when they do. Compare `reclaim`.
