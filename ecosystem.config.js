module.exports = {
  apps: [
    {
      name: "polymarket-bot",
      cwd: "C:\\Users\\Nazri Hussain\\projects\\polymarket-bot",
      script: "target\\release\\polymarket-bot.exe",
      args: "run",
      interpreter: "none",
      autorestart: true,
      watch: false,
      max_memory_restart: "500M"
    },
    {
      name: "btc5min-paper",
      cwd: "C:\\Users\\Nazri Hussain\\projects\\polymarket-bot",
      script: "target\\release\\polymarket-bot.exe",
      args: "btc5min run",
      interpreter: "none",
      autorestart: true,
      watch: false,
      max_memory_restart: "500M"
    }
  ]
}
