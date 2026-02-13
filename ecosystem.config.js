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
      max_memory_restart: "500M",
      restart_delay: 10000
    }
  ]
}
