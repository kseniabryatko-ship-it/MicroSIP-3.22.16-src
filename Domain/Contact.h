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

class Contact
{
public:
    CString name;
    CString number;
    CString firstname;
    CString lastname;
    CString phone;
    CString mobile;
    CString email;
    CString address;
    CString city;
    CString state;
    CString zip;
    CString comment;
    CString id;
    bool presence = false;
    bool starred = false;
    bool directory = false;
    CString info;
    bool ringing = false;
    int image = 0;
    bool candidate = false;
};

struct ContactWithFields {
    Contact contact;
    CStringList fields;
    bool processed = false;
};
