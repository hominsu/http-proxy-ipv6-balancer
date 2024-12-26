target "metadata" {}

group "default" {
  targets = [
    "http-proxy-ipv6-balancer",
  ]
}

target "cross" {
  platforms = [
    "linux/arm64",
    "linux/amd64"
  ]
}

target "http-proxy-ipv6-balancer" {
  inherits = ["metadata", "cross"]
  cache-from = ["type=gha"]
  cache-to = ["type=gha,mode=max"]
  context    = "."
  dockerfile = "deploy/Dockerfile"
}
