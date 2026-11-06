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

#include <memory>
#include <vector>
#include "../domain/Contact.h"
#include "../data/IContactsRepository.h"

class ContactsService
{
public:
    explicit ContactsService(std::unique_ptr<IContactsRepository> repository);

    bool IsContactFiletered(const Contact& contact, CString search);
    bool ContactPrepare(Contact& contact);

    void ExportContactsCSV(CString filename, CString search, CListCtrl* list);
    void ExportContactsXML(CString filename, CString search, CListCtrl* list);

    CString ImportContactsCSV(CString filename, CArray<ContactWithFields*>& contactsWithFields, bool directory = false);

private:
    std::unique_ptr<IContactsRepository> m_repository;

    bool Validate(const Contact& contact, CString* error = nullptr);
    
};
