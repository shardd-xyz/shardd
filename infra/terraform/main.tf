locals {
  aws_machines = {
    for name, machine in var.machines : name => machine
    if machine.provider == "aws"
  }

  aws_use1_machines = {
    for name, machine in local.aws_machines : name => machine
    if machine.region == "us-east-1"
  }

  aws_ape1_machines = {
    for name, machine in local.aws_machines : name => machine
    if machine.region == "ap-east-1"
  }

  aws_euc1_machines = {
    for name, machine in local.aws_machines : name => machine
    if machine.region == "eu-central-1"
  }
}

data "aws_ssm_parameter" "ami_use1" {
  for_each = {
    for name, machine in local.aws_use1_machines : name => machine
    if startswith(machine.provider_config.ami, "resolve:ssm:")
  }

  name = trimprefix(each.value.provider_config.ami, "resolve:ssm:")
}

data "aws_ssm_parameter" "ami_ape1" {
  provider = aws.ap_east_1

  for_each = {
    for name, machine in local.aws_ape1_machines : name => machine
    if startswith(machine.provider_config.ami, "resolve:ssm:")
  }

  name = trimprefix(each.value.provider_config.ami, "resolve:ssm:")
}

data "aws_ssm_parameter" "ami_euc1" {
  provider = aws.eu_central_1

  for_each = {
    for name, machine in local.aws_euc1_machines : name => machine
    if startswith(machine.provider_config.ami, "resolve:ssm:")
  }

  name = trimprefix(each.value.provider_config.ami, "resolve:ssm:")
}

data "aws_subnet" "use1" {
  for_each = local.aws_use1_machines
  id       = each.value.provider_config.subnet_id
}

data "aws_subnet" "ape1" {
  provider = aws.ap_east_1
  for_each = local.aws_ape1_machines
  id       = each.value.provider_config.subnet_id
}

data "aws_subnet" "euc1" {
  provider = aws.eu_central_1
  for_each = local.aws_euc1_machines
  id       = each.value.provider_config.subnet_id
}

resource "aws_security_group" "use1" {
  for_each = local.aws_use1_machines

  name        = replace(each.key, "_", "-")
  description = "Managed by shardd Terraform for ${var.display_name}"
  vpc_id      = data.aws_subnet.use1[each.key].vpc_id

  ingress {
    from_port   = 22
    to_port     = 22
    protocol    = "tcp"
    cidr_blocks = ["0.0.0.0/0"]
    description = "SSH"
  }

  dynamic "ingress" {
    for_each = toset(each.value.public_ports)

    content {
      from_port   = ingress.value
      to_port     = ingress.value
      protocol    = "tcp"
      cidr_blocks = ["0.0.0.0/0"]
      description = "shardd public port ${ingress.value}"
    }
  }

  egress {
    from_port        = 0
    to_port          = 0
    protocol         = "-1"
    cidr_blocks      = ["0.0.0.0/0"]
    ipv6_cidr_blocks = ["::/0"]
    description      = "Allow all egress"
  }

  tags = {
    Name       = each.key
    Project    = "shardd"
    Deployment = var.display_name
    ManagedBy  = "shardd-terraform"
  }
}

resource "aws_security_group" "ape1" {
  provider = aws.ap_east_1
  for_each = local.aws_ape1_machines

  name        = replace(each.key, "_", "-")
  description = "Managed by shardd Terraform for ${var.display_name}"
  vpc_id      = data.aws_subnet.ape1[each.key].vpc_id

  ingress {
    from_port   = 22
    to_port     = 22
    protocol    = "tcp"
    cidr_blocks = ["0.0.0.0/0"]
    description = "SSH"
  }

  dynamic "ingress" {
    for_each = toset(each.value.public_ports)

    content {
      from_port   = ingress.value
      to_port     = ingress.value
      protocol    = "tcp"
      cidr_blocks = ["0.0.0.0/0"]
      description = "shardd public port ${ingress.value}"
    }
  }

  egress {
    from_port        = 0
    to_port          = 0
    protocol         = "-1"
    cidr_blocks      = ["0.0.0.0/0"]
    ipv6_cidr_blocks = ["::/0"]
    description      = "Allow all egress"
  }

  tags = {
    Name       = each.key
    Project    = "shardd"
    Deployment = var.display_name
    ManagedBy  = "shardd-terraform"
  }
}

resource "aws_security_group" "euc1" {
  provider = aws.eu_central_1
  for_each = local.aws_euc1_machines

  name        = replace(each.key, "_", "-")
  description = "Managed by shardd Terraform for ${var.display_name}"
  vpc_id      = data.aws_subnet.euc1[each.key].vpc_id

  ingress {
    from_port   = 22
    to_port     = 22
    protocol    = "tcp"
    cidr_blocks = ["0.0.0.0/0"]
    description = "SSH"
  }

  dynamic "ingress" {
    for_each = toset(each.value.public_ports)

    content {
      from_port   = ingress.value
      to_port     = ingress.value
      protocol    = "tcp"
      cidr_blocks = ["0.0.0.0/0"]
      description = "shardd public port ${ingress.value}"
    }
  }

  egress {
    from_port        = 0
    to_port          = 0
    protocol         = "-1"
    cidr_blocks      = ["0.0.0.0/0"]
    ipv6_cidr_blocks = ["::/0"]
    description      = "Allow all egress"
  }

  tags = {
    Name       = each.key
    Project    = "shardd"
    Deployment = var.display_name
    ManagedBy  = "shardd-terraform"
  }
}

resource "aws_instance" "use1" {
  for_each = local.aws_use1_machines

  ami                         = startswith(each.value.provider_config.ami, "resolve:ssm:") ? data.aws_ssm_parameter.ami_use1[each.key].value : each.value.provider_config.ami
  instance_type               = each.value.provider_config.instance_type
  subnet_id                   = each.value.provider_config.subnet_id
  vpc_security_group_ids      = [aws_security_group.use1[each.key].id]
  key_name                    = each.value.provider_config.key_name
  associate_public_ip_address = true
  user_data_replace_on_change = true
  user_data = templatefile("${path.module}/templates/bootstrap.sh.tftpl", {
    remote_root          = each.value.remote_root
    deploy_user          = each.value.ssh_user
    allowed_ports_csv    = join(",", [for port in each.value.public_ports : tostring(port)])
    infra_ssh_public_key = var.infra_ssh_public_key
  })

  root_block_device {
    volume_size = each.value.provider_config.volume_size_gb
    volume_type = "gp3"
  }

  tags = {
    Name       = each.key
    Project    = "shardd"
    Deployment = var.display_name
    ManagedBy  = "shardd-terraform"
    Role       = join("-", [for service in each.value.services : service.bundle])
  }
}

resource "aws_instance" "ape1" {
  provider = aws.ap_east_1
  for_each = local.aws_ape1_machines

  ami                         = startswith(each.value.provider_config.ami, "resolve:ssm:") ? data.aws_ssm_parameter.ami_ape1[each.key].value : each.value.provider_config.ami
  instance_type               = each.value.provider_config.instance_type
  subnet_id                   = each.value.provider_config.subnet_id
  vpc_security_group_ids      = [aws_security_group.ape1[each.key].id]
  key_name                    = each.value.provider_config.key_name
  associate_public_ip_address = true
  user_data_replace_on_change = true
  user_data = templatefile("${path.module}/templates/bootstrap.sh.tftpl", {
    remote_root          = each.value.remote_root
    deploy_user          = each.value.ssh_user
    allowed_ports_csv    = join(",", [for port in each.value.public_ports : tostring(port)])
    infra_ssh_public_key = var.infra_ssh_public_key
  })

  root_block_device {
    volume_size = each.value.provider_config.volume_size_gb
    volume_type = "gp3"
  }

  tags = {
    Name       = each.key
    Project    = "shardd"
    Deployment = var.display_name
    ManagedBy  = "shardd-terraform"
    Role       = join("-", [for service in each.value.services : service.bundle])
  }
}

resource "aws_instance" "euc1" {
  provider = aws.eu_central_1
  for_each = local.aws_euc1_machines

  ami                         = startswith(each.value.provider_config.ami, "resolve:ssm:") ? data.aws_ssm_parameter.ami_euc1[each.key].value : each.value.provider_config.ami
  instance_type               = each.value.provider_config.instance_type
  subnet_id                   = each.value.provider_config.subnet_id
  vpc_security_group_ids      = [aws_security_group.euc1[each.key].id]
  key_name                    = each.value.provider_config.key_name
  associate_public_ip_address = true
  user_data_replace_on_change = true
  user_data = templatefile("${path.module}/templates/bootstrap.sh.tftpl", {
    remote_root          = each.value.remote_root
    deploy_user          = each.value.ssh_user
    allowed_ports_csv    = join(",", [for port in each.value.public_ports : tostring(port)])
    infra_ssh_public_key = var.infra_ssh_public_key
  })

  root_block_device {
    volume_size = each.value.provider_config.volume_size_gb
    volume_type = "gp3"
  }

  tags = {
    Name       = each.key
    Project    = "shardd"
    Deployment = var.display_name
    ManagedBy  = "shardd-terraform"
    Role       = join("-", [for service in each.value.services : service.bundle])
  }
}

locals {
  machine_records = merge(
    {
      for name, instance in aws_instance.use1 : name => {
        provider_id    = instance.id
        provider       = "aws"
        provider_state = "running"
        region         = var.machines[name].region
        public_ip      = instance.public_ip
        public_dns     = instance.public_dns
        private_ip     = instance.private_ip
        host           = coalesce(instance.public_ip, instance.public_dns, instance.private_ip)
      }
    },
    {
      for name, instance in aws_instance.ape1 : name => {
        provider_id    = instance.id
        provider       = "aws"
        provider_state = "running"
        region         = var.machines[name].region
        public_ip      = instance.public_ip
        public_dns     = instance.public_dns
        private_ip     = instance.private_ip
        host           = coalesce(instance.public_ip, instance.public_dns, instance.private_ip)
      }
    },
    {
      for name, instance in aws_instance.euc1 : name => {
        provider_id    = instance.id
        provider       = "aws"
        provider_state = "running"
        region         = var.machines[name].region
        public_ip      = instance.public_ip
        public_dns     = instance.public_dns
        private_ip     = instance.private_ip
        host           = coalesce(instance.public_ip, instance.public_dns, instance.private_ip)
      }
    },
  )

  cloudflare_enabled = var.cloudflare_zone_id != "" && var.cloudflare_account_id != ""

  cloudflare_dns_records = merge(
    var.cloudflare_records,
    var.cloudflare_lb_enabled ? {} : {
      for machine_name in var.cloudflare_lb_origin_order : "edge-api-${machine_name}" => {
        name         = var.edge_api_dns_name
        machine_name = machine_name
        proxied      = true
      }
    }
  )
}

resource "cloudflare_dns_record" "records" {
  for_each = local.cloudflare_enabled ? local.cloudflare_dns_records : {}

  zone_id = var.cloudflare_zone_id
  name    = each.value.name
  type    = "A"
  ttl     = each.value.proxied ? 1 : 300
  proxied = each.value.proxied
  content = local.machine_records[each.value.machine_name].public_ip
  comment = "Managed by shardd Terraform (${var.deployment_name})"
}

resource "cloudflare_load_balancer_monitor" "edge" {
  count = local.cloudflare_enabled && var.cloudflare_lb_enabled && length(var.cloudflare_lb_origins) > 0 ? 1 : 0

  account_id       = var.cloudflare_account_id
  description      = "${var.display_name} edge health"
  type             = "https"
  method           = var.cloudflare_lb_monitor.method
  path             = var.cloudflare_lb_monitor.path
  port             = var.cloudflare_lb_monitor.port
  expected_codes   = var.cloudflare_lb_monitor.expected_codes
  timeout          = var.cloudflare_lb_monitor.timeout_seconds
  interval         = var.cloudflare_lb_monitor.interval_seconds
  retries          = var.cloudflare_lb_monitor.retries
  allow_insecure   = false
  follow_redirects = true
  probe_zone       = var.cloudflare_zone_name
}

resource "cloudflare_load_balancer_pool" "edge" {
  for_each = local.cloudflare_enabled && var.cloudflare_lb_enabled ? var.cloudflare_lb_origins : {}

  account_id      = var.cloudflare_account_id
  name            = replace(each.key, "-", "_")
  enabled         = true
  minimum_origins = 1
  monitor         = cloudflare_load_balancer_monitor.edge[0].id
  description     = "shardd ${var.deployment_name} edge pool for ${each.key}"
  origins = [
    {
      address = each.value.hostname
      enabled = true
      name    = each.key
      port    = 443
      header = {
        host = [each.value.hostname]
      }
      weight = 1
    }
  ]
}

resource "cloudflare_load_balancer" "api" {
  count = local.cloudflare_enabled && var.cloudflare_lb_enabled && length(var.cloudflare_lb_origin_order) > 0 ? 1 : 0

  zone_id         = var.cloudflare_zone_id
  name            = var.edge_api_dns_name
  proxied         = true
  steering_policy = "dynamic_latency"
  default_pools   = [for machine_name in var.cloudflare_lb_origin_order : cloudflare_load_balancer_pool.edge[machine_name].id]
  fallback_pool   = cloudflare_load_balancer_pool.edge[var.cloudflare_lb_origin_order[0]].id
  description     = "shardd ${var.deployment_name} edge load balancer"
  adaptive_routing = {
    failover_across_pools = true
  }
}
