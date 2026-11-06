/*
 * Copyright (C) 2011-2026 MicroSIP (http://www.microsip.org)
 *
 * This program is free software; you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation; either version 2 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program; if not, write to the Free Software
 * Foundation, Inc., 59 Temple Place, Suite 330, Boston, MA  02111-1307  USA
 */

#pragma once
#include "define.h"

enum { MSIP_CALL_TYPE_OUTGOING, MSIP_CALL_TYPE_INCOMING, MSIP_CALL_TYPE_MISSED, MSIP_CALL_TYPE_AE};
#define MSIP_CALL_TYPE_MIN MSIP_CALL_TYPE_OUTGOING
#define MSIP_CALL_TYPE_MAX MSIP_CALL_TYPE_AE

class Call
{
public:
    int id = 0;
    CString callId;
    CString number;
    CString name;
    int type = 0;
    int64_t time = 0;
    int duration = 0;
    CString info;
};
