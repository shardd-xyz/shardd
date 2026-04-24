output "machine_records" {
  value = local.machine_records
}

output "public_dns_records" {
  value = {
    for key, record in var.cloudflare_records : key => {
      name    = record.name
      proxied = record.proxied
      target  = local.machine_records[record.machine_name].public_ip
    }
  }
}

output "edge_api_hostname" {
  value = var.edge_api_dns_name
}
