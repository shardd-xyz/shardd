#!/bin/bash
set -e
psql -U saassy -d postgres -c "CREATE DATABASE billing_db OWNER saassy;" 2>/dev/null || true
