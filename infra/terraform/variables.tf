variable "deployment_name" {
  type = string
}

variable "display_name" {
  type = string
}

variable "expected_aws_account_id" {
  type    = string
  default = ""
}

variable "dns_root_zone" {
  type = string
}

variable "cloudflare_zone_name" {
  type = string
}

variable "cloudflare_zone_id" {
  type = string
}

variable "cloudflare_account_id" {
  type = string
}

variable "cloudflare_api_token" {
  type      = string
  sensitive = true
}

variable "cloudflare_lb_enabled" {
  type = bool
}

variable "edge_api_dns_name" {
  type    = string
  default = ""
}

variable "cloudflare_lb_monitor" {
  type = any
}

variable "cloudflare_records" {
  type = map(any)
}

variable "cloudflare_lb_origins" {
  type = map(any)
}

variable "cloudflare_lb_origin_order" {
  type = list(string)
}

variable "infra_ssh_public_key" {
  type = string
}

variable "machines" {
  type = map(any)
}
