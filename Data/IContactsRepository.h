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
#include <vector>
#include <memory>
#include "../domain/Contact.h"

class IContactsRepository
{
public:
    virtual ~IContactsRepository() = default;

    virtual void BeginTransaction() = 0;
    virtual void Commit() = 0;
    virtual void Rollback() = 0;

    virtual int Add(const Contact& log) = 0;
    virtual void Update(const Contact& log) = 0;
    virtual void Delete(int id) = 0;
    virtual void DeleteAll() = 0;

    virtual std::unique_ptr<Contact> GetById(int id) = 0;
    virtual std::unique_ptr<Contact> GetByContactId(const std::string& contactId) = 0;
    virtual std::vector<std::unique_ptr<Contact>> GetAll() = 0;
};
